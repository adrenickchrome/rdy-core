//! `rdy dump` (plan Dump and load; D-69AY3, D-D2NPN, D-FR0C4): the whole project as one
//! stream of the current dump format, 3 (`rdy_store::dump`), a full dump: its header names the
//! repo, the origin as its base and the marker of the last batch, and its trailer the removal
//! facts of its applies. It holds the published log's
//! sealed batches, byte for byte, and the closed set of project files (F5): `.rdy.yaml`,
//! `.rdyignore` and the pack files in `.rdy/packs`, never `.rdy/local.yaml`. A holder's dump
//! (D-BPYN8) holds no project file: it reads neither `.rdy.yaml` nor `.rdy/packs`, and its
//! header and batches are the same as a project's. A frame whose
//! payload an applied redaction removed travels as its dropped record: its original header, its
//! apply and its residue, never its bytes (plan Redaction, D-3M81W).
//!
//! An increment (D-2FMRR) holds the sealed batches after the end of an earlier dump or
//! increment of the same store: its header names that end, the marker of the batch that ends
//! there and that file's trailer digest. Each batch keeps its source position. When the store has
//! no batch past that end, there is nothing to write and no file is made.
//!
//! A dump is atomic at the target: it writes `<out>.partial-<pid>`, syncs it, and hard-links it
//! to `out`, so `out` is a complete dump or nothing. It never overwrites a file. A dump is of one
//! redaction epoch: a state read before a removal, or one whose removal lands during the dump, is
//! `E_REDACTED_STATE`, and nothing is placed (TASK-293).
use crate::config_levels::PROJECT_FILE;
use crate::dump_error;
use crate::error::{E_ALREADY_EXISTS, E_BAD_INPUT, E_IO, RdyError};
use crate::handle::io;
use crate::load::{open_dump, read_error};
use crate::pack_load::pack_files;
use crate::read_state::ReadState;
use crate::wc::ignore::IGNORE_FILE;
use rdy_model::{Hash, RepoId};
use rdy_store::StoreError;
use rdy_store::dump::{
    DUMP_FORMAT, DumpHeader, DumpReader, DumpTotals, DumpWriter, MAX_FILE_BYTES, MAX_FILES,
    RemovalTally, is_project_file, read_envelope,
};
use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader, DumpKind};
use rdy_store::format::STORE_FORMAT;
use rdy_store::fsutil::sync_dir;
use rdy_store::holder::StoreMode;
use rdy_store::segment::Location;
use rdy_store::transition::hex;
use serde::Serialize;
use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Read, Seek, Write};
use std::path::Path;

/// The directory of the pack files a dump carries, relative to the project root.
const PACKS_DIR: &str = ".rdy/packs";

/// What `rdy dump` wrote: the file (none when an increment had no batch to hold), its formats,
/// its kind, its chain facts (repo, base, end, end marker and trailer digest), its removal facts,
/// what it holds and its length. In JSON every hash is 64 lowercase hex digits and the repo id
/// 32.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DumpSummary {
    pub out: Option<String>,
    pub dump_format: u32,
    pub kind: DumpKind,
    pub store_format: u32,
    #[serde(serialize_with = "ids::repo")]
    pub repo_id: RepoId,
    pub base: Location,
    pub end: Location,
    pub end_batch: BatchMarker,
    #[serde(serialize_with = "hex::one::serialize")]
    pub digest: Hash,
    pub removals: u32,
    pub first_removed: Option<Location>,
    pub batches: u64,
    pub frames: u64,
    pub files: Vec<String>,
    pub bytes: u64,
}

impl DumpSummary {
    /// Where the chain ends after this file: the next increment follows this.
    pub fn chain_end(&self) -> ChainEnd {
        ChainEnd {
            repo_id: self.repo_id,
            store_format: self.store_format,
            end: self.end,
            end_batch: self.end_batch,
            digest: self.digest,
        }
    }
}

/// The serializers of repo ids in reply types: 32 lowercase hex digits, or null.
pub(crate) mod ids {
    use rdy_model::RepoId;
    use serde::Serializer;

    pub(crate) fn repo<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(r)
    }

    pub(crate) fn opt_repo<S: Serializer>(r: &Option<RepoId>, s: S) -> Result<S::Ok, S::Error> {
        match r {
            Some(r) => s.collect_str(r),
            None => s.serialize_none(),
        }
    }
}

/// A test hook a dump calls after each batch it writes, with the count written so far.
#[doc(hidden)]
pub type BatchHook = Box<dyn FnMut(u64)>;

/// A test hook a dump calls with each project file's path, after the file's length passed the
/// limit and before its first byte is read.
#[doc(hidden)]
pub type FileHook = Box<dyn FnMut(&Path)>;

thread_local! {
    static FILE_READS: Cell<u64> = const { Cell::new(0) };
    static FILE_BYTES: Cell<u64> = const { Cell::new(0) };
    static FILE_HOOK: RefCell<Option<FileHook>> = const { RefCell::new(None) };
    static BATCH_HOOK: RefCell<Option<BatchHook>> = const { RefCell::new(None) };
}

/// Test hook: this thread's count of project files a dump has opened to read.
#[doc(hidden)]
pub fn project_file_reads() -> u64 {
    FILE_READS.with(Cell::get)
}

/// Test hook: this thread's count of project-file bytes a dump has read from the files.
#[doc(hidden)]
pub fn project_file_bytes() -> u64 {
    FILE_BYTES.with(Cell::get)
}

/// Test hook: a dump on this thread calls `hook` with each project file's path, after its
/// length is checked and before its first byte is read (a test can grow the file there). None
/// removes the hook.
#[doc(hidden)]
pub fn on_project_file(hook: Option<FileHook>) {
    FILE_HOOK.with(|h| *h.borrow_mut() = hook);
}

/// A project file's reader that counts the bytes it hands out ([`project_file_bytes`]).
struct Counted(File);

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        FILE_BYTES.with(|c| c.set(c.get() + n as u64));
        Ok(n)
    }
}

/// Test hook: a dump on this thread calls `hook` with the count of batches written so far,
/// after each batch it writes (a read barrier for race tests). None removes the hook.
#[doc(hidden)]
pub fn on_dump_batch(hook: Option<BatchHook>) {
    BATCH_HOOK.with(|h| *h.borrow_mut() = hook);
}

/// The project files a dump carries (F5, a closed set), with their bytes, in ascending path
/// order: `.rdy.yaml` (required), `.rdyignore` if present, and each `.rdy/packs` file that the
/// pack loader reads. A pack file whose name is outside the set is refused, and so is a file
/// larger than 16 MiB, before its bytes are read.
pub fn project_files(root: &Path) -> Result<Vec<(String, Vec<u8>)>, RdyError> {
    let mut files = vec![(
        PROJECT_FILE.to_string(),
        read_named(&root.join(PROJECT_FILE), PROJECT_FILE)?,
    )];
    if let Some(bytes) = read_bounded(&root.join(IGNORE_FILE), IGNORE_FILE)? {
        files.push((IGNORE_FILE.to_string(), bytes));
    }
    for f in pack_files(&root.join(PACKS_DIR))? {
        let name = f.file_name().unwrap_or_default();
        let path = format!("{PACKS_DIR}/{}", name.to_string_lossy());
        if name.to_str().is_none() || !is_project_file(&path) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "{path}: rdy dump carries pack files named with A-Z a-z 0-9 . _ - only, and not starting with a dot"
                ),
                "rename the pack file",
            ));
        }
        let bytes = read_named(&f, &path)?;
        files.push((path, bytes));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// The chain end of the dump file `file`, open as `r`, from its envelope only: its header is read
/// first, and a dump of format 1 or 2, which holds no chain facts, is refused. For format 3 the
/// header and the final trailer are read (`read_envelope`); the body between them is not.
/// A malformed envelope is the load's `E_DUMP` at its byte.
pub fn chain_end_of<R: Read + Seek>(file: &Path, r: &mut R) -> Result<ChainEnd, RdyError> {
    let (_, format, _) = DumpReader::open(&mut *r).map_err(|e| read_error(file, e))?;
    if format < 3 {
        return Err(dump_error::export_legacy_predecessor(Some(file), format));
    }
    let (format, header, trailer, _) = read_envelope(r).map_err(|e| read_error(file, e))?;
    let Some(chain) = header.chain else {
        return Err(dump_error::export_legacy_predecessor(Some(file), format));
    };
    Ok(ChainEnd {
        repo_id: chain.repo_id,
        store_format: header.store_format,
        end: header.log_end,
        end_batch: chain.end_batch,
        digest: trailer.digest,
    })
}

impl ReadState {
    /// Writes this state as a full dump to `out`: the project files, then every sealed batch of
    /// the log up to `seq()`. Refuses an `out` that exists, an `out` whose directory does not,
    /// and an `out` in the project's `.rdy` directory.
    pub fn dump(&self, out: &Path) -> Result<DumpSummary, RdyError> {
        self.dump_to(out, None)
    }

    /// Writes this state as an increment to `out`: the project files, then the sealed batches
    /// after `prev`, the end of an earlier dump or increment of this store, up to `seq()`. The
    /// predecessor is checked first, in this order: its repo, its store format, an end at or
    /// before this state's, an end that closes a batch of this store, and that batch's marker.
    /// Its errors name it `previous dump`. A removal that lands while the marker is looked up is
    /// `E_REDACTED_STATE`, never a predecessor error. When this state ends at `prev`, nothing is
    /// written, no project file is read, and the summary has no `out` and names `prev`'s end.
    pub fn dump_after(&self, prev: &ChainEnd, out: &Path) -> Result<DumpSummary, RdyError> {
        self.dump_following(None, prev, out)
    }

    /// [`ReadState::dump_after`] the dump file `after`, whose envelope alone is read
    /// ([`chain_end_of`]); its errors name the file.
    pub fn dump_after_file(&self, after: &Path, out: &Path) -> Result<DumpSummary, RdyError> {
        self.ensure_current_epoch()?;
        let mut f = open_dump(after)?;
        let prev = chain_end_of(after, &mut f)?;
        drop(f);
        self.dump_following(Some(after), &prev, out)
    }

    /// The increment after `prev`; `file` names the predecessor in the texts (None: the
    /// `previous dump`).
    fn dump_following(
        &self,
        file: Option<&Path>,
        prev: &ChainEnd,
        out: &Path,
    ) -> Result<DumpSummary, RdyError> {
        self.ensure_current_epoch()?;
        let repo_id = repo_id_of(self)?;
        if prev.repo_id != repo_id {
            return Err(dump_error::export_repo_mismatch(
                file,
                &prev.repo_id,
                &repo_id,
            ));
        }
        if prev.store_format != STORE_FORMAT {
            return Err(dump_error::export_store_mismatch(
                file,
                prev.store_format,
                STORE_FORMAT,
            ));
        }
        let at = prev.end;
        if at > self.seq() {
            return Err(dump_error::export_end_past_state(
                file, at.segment, at.offset,
            ));
        }
        let found = self.marker_at(at);
        if !matches!(found, Ok(Some(marker)) if marker == prev.end_batch) {
            // a removal that lands during the walk breaks it, or bends what it reads: that
            // answer is no one epoch's, so the epoch's error comes first
            self.ensure_current_epoch()?;
        }
        let Some(marker) = found? else {
            return Err(dump_error::export_end_inside_batch(
                file, at.segment, at.offset,
            ));
        };
        if marker != prev.end_batch {
            return Err(dump_error::export_marker_mismatch(
                file, at.segment, at.offset,
            ));
        }
        if at == self.seq() {
            // no batch to hold: no file, and no project file read; still one epoch's answer
            self.ensure_current_epoch()?;
            return Ok(DumpSummary {
                out: None,
                dump_format: DUMP_FORMAT,
                kind: DumpKind::Increment,
                store_format: STORE_FORMAT,
                repo_id,
                base: at,
                end: at,
                end_batch: prev.end_batch,
                digest: prev.digest,
                removals: 0,
                first_removed: None,
                batches: 0,
                frames: 0,
                files: Vec::new(),
                bytes: 0,
            });
        }
        self.dump_to(out, Some(prev))
    }

    /// The marker of the batch of this store that ends at `at`, or None when `at` closes no
    /// batch: a place inside a frame or a batch, or offset zero of a segment (the origin
    /// included). `at` is at or before `seq()`, inside the checked log, so a walk to it that
    /// meets damage met a place that is not a frame boundary.
    fn marker_at(&self, at: Location) -> Result<Option<BatchMarker>, RdyError> {
        match self.reader().batch_marker_at(at) {
            Ok(marker) => Ok(marker),
            Err(StoreError::Decode(_)) | Err(StoreError::RedactionCorrupt { .. }) => Ok(None),
            Err(StoreError::Io(e))
                if matches!(e.kind(), ErrorKind::InvalidData | ErrorKind::UnexpectedEof) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Writes the dump (`prev` None) or the increment after `prev` to `out`, through the
    /// output guards and [`place`].
    fn dump_to(&self, out: &Path, prev: Option<&ChainEnd>) -> Result<DumpSummary, RdyError> {
        if std::fs::symlink_metadata(out).is_ok() {
            return Err(exists(out, DUMP_CMD));
        }
        let dir = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !dir.exists() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{} does not exist", dir.display()),
                "create the directory first, or pick another path",
            ));
        }
        inside_rdy(self.root(), dir, out)?;
        let files = self.dump_files()?;
        let (totals, chain) = place(out, DUMP_CMD, |partial, file| {
            self.write_chain(BufWriter::new(file), &files, prev)
                .and_then(|(buf, t, chain)| {
                    let file = buf
                        .into_inner()
                        .map_err(|e| io_at(partial, e.into_error()))?;
                    file.sync_all().map_err(|e| io_at(partial, e))?;
                    Ok((t, chain))
                })
        })?;
        Ok(DumpSummary {
            out: Some(out.display().to_string()),
            dump_format: DUMP_FORMAT,
            kind: chain.kind,
            store_format: STORE_FORMAT,
            repo_id: chain.repo_id,
            base: chain.base,
            end: self.seq(),
            end_batch: chain.end_batch,
            digest: totals.digest,
            removals: totals.removals,
            first_removed: totals.first_removed,
            batches: totals.batches,
            frames: totals.frames,
            files: files.into_iter().map(|(path, _)| path).collect(),
            bytes: totals.bytes,
        })
    }

    /// The project files this dump carries: a project's closed set, and none for a holder (it
    /// has no config file and no pack directory, and neither is read).
    fn dump_files(&self) -> Result<Vec<(String, Vec<u8>)>, RdyError> {
        match self.reader().mode()? {
            StoreMode::Holder => Ok(Vec::new()),
            StoreMode::Project => project_files(self.root()),
        }
    }

    /// Writes the whole dump into `w` and returns `w`, flushed (a dump, and a clone's stream).
    /// See [`ReadState::write_chain`].
    pub(crate) fn write_dump<W: Write>(
        &self,
        w: W,
        files: &[(String, Vec<u8>)],
    ) -> Result<(W, DumpTotals), RdyError> {
        self.write_chain(w, files, None).map(|(w, t, _)| (w, t))
    }

    /// Writes the full dump (`prev` None), or the increment after `prev`, into `w`, and returns
    /// `w`, its totals and its chain facts. An increment's batches are those of `(prev.end,
    /// seq()]`, each at its source position; the caller has checked `prev` against this store.
    ///
    /// The dump belongs to this state's redaction epoch (TASK-293): a state read before a
    /// removal, or one whose removal lands while the batches are written, is `E_REDACTED_STATE`.
    /// The epoch is checked before anything is written and again after the last batch is read,
    /// before the trailer (so before a dump file is placed, and before a clone's loader can read
    /// a whole stream): the batches read the live removal spans, and a removal past this state's
    /// log end would leave a dropped frame that no record of the dump authorizes. A walk that
    /// fails is checked too, the marker lookup at the log end included, so a removal that broke
    /// it is `E_REDACTED_STATE`. The caller removes its partial output on any error.
    fn write_chain<W: Write>(
        &self,
        w: W,
        files: &[(String, Vec<u8>)],
        prev: Option<&ChainEnd>,
    ) -> Result<(W, DumpTotals, DumpChainHeader), RdyError> {
        self.ensure_current_epoch()?;
        let reader = self.reader();
        let repo_id = repo_id_of(self)?;
        // the marker of the batch that ends at this state's log end; a removal that breaks this
        // walk is the epoch's error, as for the batches below
        let end_batch = match reader.batch_marker_at(self.seq()) {
            Ok(marker) => marker.unwrap_or(BatchMarker::ZERO),
            Err(e) => {
                self.ensure_current_epoch()?;
                return Err(e.into());
            }
        };
        let (chain, base) = match prev {
            None => (
                DumpChainHeader::full(repo_id, end_batch),
                BatchMarker::ZERO.start,
            ),
            Some(prev) => (DumpChainHeader::after(prev, end_batch), prev.end),
        };
        let header = DumpHeader {
            store_format: STORE_FORMAT,
            log_end: self.seq(),
            // the workspace version: the header names the rdy that wrote the dump (O17)
            rdy: env!("CARGO_PKG_VERSION").to_string(),
            chain: Some(chain),
        };
        check_limits(files)?;
        // the writer's i/o errors are the destination's (disk full, say), never the store's
        let mut w = DumpWriter::new(w, &header).map_err(dest)?;
        for (path, bytes) in files {
            w.file(path, bytes).map_err(dest)?;
        }
        let mut failed = None;
        let mut facts = RemovalTally::default();
        let mut written = 0u64;
        let read = reader.for_each_batch_after(base, self.seq(), &mut |b| {
            // the source walker's applies; their spans are resolved by the store below
            facts.observe(&b)?;
            w.batch(&b).map_err(|e| {
                let stop = StoreError::Refused(e.to_string());
                failed = Some(dest(e));
                stop
            })?;
            written += 1;
            BATCH_HOOK.with(|h| {
                if let Some(hook) = h.borrow_mut().as_mut() {
                    hook(written);
                }
            });
            Ok(())
        });
        let read = match failed {
            Some(e) => Err(e),
            None => read.map_err(RdyError::from),
        };
        // each apply's resolved local spans, as the store's authorization has them
        let facts = read.and_then(|()| Ok(facts.resolved(reader, self.seq())?));
        // a removal that landed during the batches: what they read is no one epoch's, and a
        // walk it broke fails for that reason
        self.ensure_current_epoch()?;
        let facts = facts?;
        let (w, totals) = w.finish(facts).map_err(dest)?;
        Ok((w, totals, chain))
    }
}

/// The repo id of the store `st` reads.
fn repo_id_of(st: &ReadState) -> Result<RepoId, RdyError> {
    Ok(st
        .reader()
        .repo_id()?
        .ok_or_else(|| StoreError::Decode("the store has no meta/repo_id; run rdy fsck".into()))?)
}

/// The command `rdy dump` names in its texts.
const DUMP_CMD: &str = "rdy dump";

/// Places a file at `out` as `rdy dump` and `rdy export` do: `out` must not exist and its
/// directory must. The bytes go to `<out>.partial-<pid>` (`create_new`), which is synced, then
/// hard-linked to `out`, so `out` is the whole file or nothing and never replaces a file. The
/// partial is removed and the directory synced. `cmd` names the command in the texts
/// (`rdy dump` or `rdy export`). Returns the length of the file.
pub fn place_file(out: &Path, bytes: &[u8], cmd: &str) -> Result<u64, RdyError> {
    place(out, cmd, |partial, mut file| {
        file.write_all(bytes).map_err(|e| io_at(partial, e))?;
        file.sync_all().map_err(|e| io_at(partial, e))?;
        Ok(bytes.len() as u64)
    })
}

/// The write rule of [`place_file`] with the filling of the partial file left to `fill`, which
/// gets the partial's path and the file, and syncs it.
fn place<T>(
    out: &Path,
    cmd: &str,
    fill: impl FnOnce(&Path, std::fs::File) -> Result<T, RdyError>,
) -> Result<T, RdyError> {
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(exists(out, cmd));
    }
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !dir.exists() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} does not exist", dir.display()),
            "create the directory first, or pick another path",
        ));
    }
    let mut partial = out.as_os_str().to_owned();
    partial.push(format!(".partial-{}", std::process::id()));
    let partial = Path::new(&partial);
    // only a partial file this call created is ever removed
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial)
        .map_err(|e| match e.kind() {
            // never removed: it is another call's, or left by a call that was killed
            ErrorKind::AlreadyExists => RdyError::new(
                E_ALREADY_EXISTS,
                format!(
                    "{} exists: another {} may be writing it",
                    partial.display(),
                    cmd.strip_prefix("rdy ").unwrap_or(cmd)
                ),
                format!(
                    "remove it if no {} is running, then retry",
                    cmd.strip_prefix("rdy ").unwrap_or(cmd)
                ),
            ),
            _ => io_at(partial, e),
        })?;
    let done = fill(partial, file).and_then(|t| {
        link_into_place(partial, out, cmd, |a, b| std::fs::hard_link(a, b))?;
        Ok(t)
    });
    let done = match done {
        Ok(done) => done,
        Err(e) => {
            let _ = std::fs::remove_file(partial);
            return Err(e);
        }
    };
    std::fs::remove_file(partial).map_err(|e| io_at(partial, e))?;
    sync_dir(dir).map_err(|e| io_at(dir, e))?;
    Ok(done)
}

/// Links the synced `partial` to `out` with `link` (`std::fs::hard_link`): `out` appears whole
/// or not at all, and never over a file. A file system without hard links (FAT, exFAT, some
/// network mounts) gets its own fix: a retry there never passes. `cmd` names the command.
fn link_into_place(
    partial: &Path,
    out: &Path,
    cmd: &str,
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), RdyError> {
    link(partial, out).map_err(|e| {
        if e.kind() == ErrorKind::AlreadyExists {
            exists(out, cmd)
        } else if no_hard_links(&e) {
            let (noun, verb) = if cmd == DUMP_CMD {
                ("the dump", "dump")
            } else {
                ("the file", cmd.strip_prefix("rdy ").unwrap_or(cmd))
            };
            RdyError::new(
                E_IO,
                format!(
                    "{}: this file system does not support hard links, which {cmd} uses to put {noun} in place",
                    out.display()
                ),
                format!("{verb} to a local disk, then copy the file"),
            )
        } else {
            io_at(out, e)
        }
    })
}

/// Whether `link(2)` failed because the file system has no hard links: `EOPNOTSUPP` (Linux, and
/// any `Unsupported`), `EPERM` (Linux FAT; the partial file was just created in that directory,
/// so it is not a permission matter), and `ENOTSUP` (macOS FAT and exFAT).
fn no_hard_links(e: &std::io::Error) -> bool {
    const EPERM: i32 = 1;
    const MACOS_ENOTSUP: i32 = 45;
    e.kind() == ErrorKind::Unsupported
        || e.raw_os_error() == Some(EPERM)
        || (cfg!(target_vendor = "apple") && e.raw_os_error() == Some(MACOS_ENOTSUP))
}

/// Refuses an `out` in the project's `.rdy` directory or below it: a file there can break the
/// store (a stray pack file, a segment name). Compares resolved paths, so a symlink or `..`
/// does not pass.
fn inside_rdy(root: &Path, dir: &Path, out: &Path) -> Result<(), RdyError> {
    let rdy = root.join(".rdy");
    let rdy = std::fs::canonicalize(&rdy).map_err(|e| io_at(&rdy, e))?;
    let dir = std::fs::canonicalize(dir).map_err(|e| io_at(dir, e))?;
    if dir.starts_with(&rdy) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} is inside the project .rdy directory", out.display()),
            "dump to a path outside .rdy",
        ));
    }
    Ok(())
}

/// Refuses more project files than a dump carries, and a file larger than a dump carries, each
/// with its own fix: a retry never passes these.
fn check_limits(files: &[(String, Vec<u8>)]) -> Result<(), RdyError> {
    if files.len() > MAX_FILES as usize {
        let others = files
            .iter()
            .filter(|(p, _)| !p.starts_with(&format!("{PACKS_DIR}/")))
            .count();
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "the project has {} files to dump, and rdy dump carries at most {MAX_FILES}",
                files.len()
            ),
            format!("keep at most {} pack files", MAX_FILES as usize - others),
        ));
    }
    if let Some((path, _)) = files.iter().find(|(_, b)| b.len() > MAX_FILE_BYTES) {
        return Err(too_large(path));
    }
    Ok(())
}

/// An error of the dump writer: its i/o errors are the destination's.
fn dest(e: StoreError) -> RdyError {
    match e {
        StoreError::Io(e) => io(e),
        e => e.into(),
    }
}

/// Reads one project file, `name` in the dump; an error names it.
fn read_named(path: &Path, name: &str) -> Result<Vec<u8>, RdyError> {
    match read_bounded(path, name)? {
        Some(bytes) => Ok(bytes),
        None => Err(io_at(path, std::io::Error::from(ErrorKind::NotFound))),
    }
}

/// Reads one project file, `name` in the dump, or None when it does not exist. A file larger
/// than a dump carries is refused by its length before a byte is read, and a file that grows
/// past it is refused at its first byte over the limit: no buffer is sized from a claim.
fn read_bounded(path: &Path, name: &str) -> Result<Option<Vec<u8>>, RdyError> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_at(path, e)),
    };
    FILE_READS.with(|n| n.set(n.get() + 1));
    let limit = MAX_FILE_BYTES as u64;
    if f.metadata().map_err(|e| io_at(path, e))?.len() > limit {
        return Err(too_large(name));
    }
    FILE_HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook(path);
        }
    });
    let mut bytes = Vec::new();
    Counted(f)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_at(path, e))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(too_large(name));
    }
    Ok(Some(bytes))
}

/// The project file `name` is larger than a dump carries.
fn too_large(name: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{name} is larger than 16 MiB"),
        "make the file 16 MiB or smaller",
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

/// `out` exists: neither `rdy dump` nor `rdy export` overwrites a file.
fn exists(out: &Path, cmd: &str) -> RdyError {
    if cmd == DUMP_CMD {
        return RdyError::new(
            E_ALREADY_EXISTS,
            format!("{} exists", out.display()),
            "pick a new file name; rdy dump never overwrites a file",
        );
    }
    RdyError::new(
        E_ALREADY_EXISTS,
        format!("{} exists; {cmd} never overwrites a file", out.display()),
        "pick another path, or remove the file",
    )
}

#[cfg(test)]
#[path = "dump_epoch_tests.rs"]
mod epoch_tests;

#[cfg(test)]
mod tests {
    use crate::Rdy;
    use crate::error::E_IO;
    use std::io::Write;

    /// A sink that takes `room` bytes, then fails as a full disk does.
    #[derive(Debug)]
    struct Full {
        room: usize,
    }

    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.room == 0 {
                return Err(std::io::Error::from_raw_os_error(28));
            }
            let n = buf.len().min(self.room);
            self.room -= n;
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn dl5_a_file_system_without_hard_links_gets_its_own_fix() {
        let out = std::path::Path::new("/mnt/stick/p.rdydump");
        let partial = std::path::Path::new("/mnt/stick/p.rdydump.partial-1");
        let mut no_links = vec![
            std::io::Error::from(std::io::ErrorKind::Unsupported),
            // Linux FAT and exFAT: EPERM
            std::io::Error::from_raw_os_error(1),
        ];
        if cfg!(target_vendor = "apple") {
            // macOS FAT and exFAT: ENOTSUP
            no_links.push(std::io::Error::from_raw_os_error(45));
        }
        for e in no_links {
            let shown = e.to_string();
            let got = super::link_into_place(partial, out, "rdy dump", |_, _| Err(e)).unwrap_err();
            assert_eq!(got.code, E_IO, "{shown}");
            assert_eq!(
                got.message,
                "/mnt/stick/p.rdydump: this file system does not support hard links, which rdy dump uses to put the dump in place",
                "{shown}"
            );
            assert_eq!(
                got.fix, "dump to a local disk, then copy the file",
                "{shown}"
            );
        }
    }

    #[test]
    fn ec7_a_file_system_without_hard_links_gets_the_export_fix() {
        let out = std::path::Path::new("/mnt/stick/p.html");
        let partial = std::path::Path::new("/mnt/stick/p.html.partial-1");
        let got = super::link_into_place(partial, out, "rdy export", |_, _| {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        })
        .unwrap_err();
        assert_eq!(got.code, E_IO);
        assert_eq!(
            got.message,
            "/mnt/stick/p.html: this file system does not support hard links, which rdy export uses to put the file in place"
        );
        assert_eq!(got.fix, "export to a local disk, then copy the file");
        let got = super::link_into_place(partial, out, "rdy export", |_, _| {
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .unwrap_err();
        assert_eq!(got.code, crate::error::E_ALREADY_EXISTS);
        assert_eq!(
            got.message,
            "/mnt/stick/p.html exists; rdy export never overwrites a file"
        );
        assert_eq!(got.fix, "pick another path, or remove the file");
    }

    #[test]
    fn dl5_a_late_io_error_names_its_path() {
        let out = std::path::Path::new("/d/p.rdydump");
        let partial = std::path::Path::new("/d/p.rdydump.partial-1");
        let e = std::io::Error::from_raw_os_error(28);
        let shown = e.to_string();
        let got = super::link_into_place(partial, out, "rdy dump", |_, _| Err(e)).unwrap_err();
        assert_eq!(got.code, E_IO);
        assert_eq!(got.message, format!("/d/p.rdydump: {shown}"));
        assert_eq!(got.fix, "check file permissions and disk space");
        let got = super::link_into_place(partial, out, "rdy dump", |_, _| {
            Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        })
        .unwrap_err();
        assert_eq!(got.code, crate::error::E_ALREADY_EXISTS);
        assert_eq!(got.message, "/d/p.rdydump exists");
    }

    #[test]
    fn dl5_a_full_disk_is_an_io_error_of_the_destination_not_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        crate::testkit::generate(dir.path(), 40);
        let r = Rdy::open(dir.path()).unwrap();
        let st = r.read();
        let files = super::project_files(dir.path()).unwrap();
        let (whole, totals) = st.write_dump(Vec::new(), &files).unwrap();
        assert!(totals.batches > 1, "a log of several batches");
        // the header, a project file, the batches, and the trailer
        for room in [0, 20, 200, whole.len() / 2, whole.len() - 1] {
            let e = st.write_dump(Full { room }, &files).unwrap_err();
            assert_eq!(e.code, E_IO, "room {room}: {e:?}");
            assert_eq!(
                e.fix, "check file permissions and disk space",
                "room {room}: {e:?}"
            );
        }
    }
}
