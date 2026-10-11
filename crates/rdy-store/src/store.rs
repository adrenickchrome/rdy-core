use crate::error::StoreError;
use crate::format::{self, Format};
use crate::fsutil;
use crate::index::{Index, IndexEntry};
use crate::kinds::{self, BlobManifest, RefUpdate};
use crate::merge::Conflict;
use crate::segment::{
    CrashPoint, Location, SegmentLog, SegmentReader, find_gap, find_unsealed, seg_path,
};
use crate::view::ViewOp;
use rdy_model::{
    Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, StateSnapshot, Tracking,
};
use serde::{Serialize, de::DeserializeOwned};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The file a store creates when it opens and removes on a clean close. Finding it at open means the
/// last process died with the store open.
pub const INDEX_OPEN_SENTINEL: &str = "index.open";

/// Where `reindex` builds the new index before it renames it over `index.redb`.
const INDEX_TMP: &str = "index.redb.tmp";

const SEGMENT_MAX: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;

/// The largest asset `rdy_asset` accepts. The store does not enforce it: only `put_blob` writes a
/// manifest-kind object, so no manifest whose bytes an agent chose is ever read as a blob.
pub const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

/// What one `Store::open` did, for tests, `rdy` diagnostics and the QA gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// Torn or unacknowledged tails this open moved aside (the acknowledged boundary is the last
    /// valid batch marker).
    pub quarantined: Vec<PathBuf>,
    /// The `index.open` sentinel was present: the last process did not close the store.
    pub unclean_shutdown: bool,
    /// redb repaired the index at open or in its integrity check.
    pub redb_repaired: bool,
    /// The index was set aside and rebuilt from the whole log.
    pub index_rebuilt: bool,
    /// Frames replayed into the index by this open.
    pub caught_up_frames: u64,
    /// Index commits the catch-up made.
    pub catch_up_commits: u32,
}

/// The read half of a store (rdr1): the index, the segments read by position, the set cache and
/// the frame counter, all shared. A clone is cheap, `Send` and `Sync`, and reads only what the
/// index holds, which the writer applies after the log's sync, so it never sees a torn frame.
/// A reader cloned before a `reindex` keeps the index it was cloned with.
#[derive(Clone)]
pub struct StoreReader {
    index: Arc<Index>,
    shared: Arc<ReaderShared>,
    /// trk1: the project pack's tracking fields, which the freeze checks let change on a frozen
    /// decision. Empty until the engine calls [`Store::set_tracking`].
    tracking: Arc<Tracking>,
}

/// What every reader of one store shares, whatever index it holds.
struct ReaderShared {
    dir: PathBuf,
    segments: SegmentReader,
    /// Per view, the head last read by `included` and the set it gave (chain1). A write never
    /// updates it: the next read walks back to the cached head or to a checkpoint. `reindex`
    /// clears it.
    sets: Mutex<HashMap<String, CachedSet>>,
    /// Object frames read by `get_raw` and `get_kind` (a test hook; reads take `&self`).
    frames_read: AtomicU64,
}

pub struct Store {
    dir: PathBuf,
    report: OpenReport,
    log: SegmentLog,
    /// The store's own reader, which holds the only store-side `Arc` of the index. None only once
    /// the clean close (`close` or `Drop`) has dropped it.
    reader: Option<StoreReader>,
    /// Declared after `reader`, so the database is closed before the lock is released.
    _lock: HeldLock,
    faults: Faults,
    /// Set when a failed write could not be rolled back; every later write fails until reopen.
    ///
    /// A known double fault (final review, Minor 10): if a write fails after its batch was
    /// flushed with its marker (its index update failed) and the rollback's truncate then fails
    /// too, the batch stays in the log, marked, and the next open accepts it as acknowledged
    /// although this write returned an error. Under M1 the durable index was the boundary, so the
    /// same double fault left the batch out. Nothing acknowledged is lost either way; the caller
    /// of the failed write should read before retrying.
    poisoned: bool,
    /// Directories open fsynced because it created something in them (dur1).
    synced: Vec<PathBuf>,
    /// Set only at the end of a successful open, after the sentinel exists, and cleared by the
    /// clean close: gates `close` and `Drop`, so a store whose open failed neither removes a
    /// kill's sentinel nor does anything else on drop, and a store closes once.
    opened: bool,
    /// Frames written since the last checkpoint.
    pending: u64,
    /// When the last write finished, for `checkpoint_if_idle`.
    last_write: Instant,
    /// A write checkpoints once `pending` reaches this.
    checkpoint_frames: u64,
    /// Checkpoints this store has made.
    checkpoints: u64,
    /// The last checkpoint a write or `checkpoint_if_idle` attempted and could not make; cleared
    /// by the next success.
    checkpoint_error: Option<String>,
    /// Successful `write` calls since open (a test hook, atom1).
    writes: u64,
    /// The kind and hash of every data frame the last successful `write` appended, in order.
    last_batch: Vec<(u8, Hash)>,
    /// The end of the log right after the last successful `write`.
    last_batch_end: Location,
}

impl Deref for Store {
    type Target = StoreReader;

    fn deref(&self) -> &StoreReader {
        self.reader.as_ref().expect("the store is open until close")
    }
}

/// The start of the log.
const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// A view's head and the included set the walk gave at it (chain1).
pub(crate) type CachedSet = (Hash, Arc<Vec<Hash>>);

/// How an index open went: the index, its acknowledged boundary (None if it has none yet), whether
/// it was set aside and replaced by an empty one, and whether redb repaired it.
struct OpenedIndex {
    index: Index,
    acked: Option<Location>,
    rebuilt: bool,
    repaired: bool,
}

/// Opens the index, verifies its integrity and reads its acknowledged boundary. `unclean` says the
/// `index.open` sentinel was present: the last process died with the store open.
///
/// - Damage is an error, a panic (redb can panic on a corrupt page), or a file redb reports as not
///   clean when the last close was clean. It is renamed to `index.redb.damaged-<n>` and replaced
///   by an empty one, which the caller rebuilds from the log; the boundary is then unknown.
/// - Anything else is a rollback: a clean file, or any file after a kill, where redb discarding
///   its non-durable commits is expected. Its last durable commit never covers an unacknowledged
///   frame, so it is trusted and the caller catches it up from its `indexed_upto`.
///
/// What redb 4.3 actually reports (final review; pinned by the index test
/// `kill1_redb_repairs_a_copy_of_a_live_index_at_open_and_then_reports_it_clean`): a killed file
/// is repaired at open (the repair callback fires) and `check_integrity` then returns clean, so a
/// kill takes the trusted arm through `clean`. Damage shows up as an error or a panic: a sweep of
/// 10,895 single-byte flips gave neither a kill nor damage as `Ok(clean == false)`. The
/// `Ok(clean == false)` arm, the only one the sentinel decides, is therefore defensive and
/// unobserved; the sentinel is reported (`unclean_shutdown`), not decisive, with redb 4.3.
///
/// A file another live database holds (a `StoreReader` that outlived its store keeps the index
/// open) is neither: the open is refused as `HeldByReader(dir)` and the file left alone.
fn open_index(dir: &Path, unclean: bool) -> Result<OpenedIndex, StoreError> {
    let path = dir.join("index.redb");
    let opened = std::panic::catch_unwind(|| {
        Index::open_checked(&path).and_then(|c| {
            let acked = c.index.acknowledged()?;
            Ok((c, acked))
        })
    })
    .unwrap_or_else(|_| Err(StoreError::Db("index panicked while opening".into())));
    match opened {
        Ok((c, acked)) if c.clean || unclean => Ok(OpenedIndex {
            repaired: !c.clean || c.repaired_at_open,
            index: c.index,
            acked,
            rebuilt: false,
        }),
        Ok((c, _)) => {
            drop(c); // close the file before it is renamed
            rebuild_index(&path, true)
        }
        Err(StoreError::HeldByReader(_)) => Err(StoreError::HeldByReader(dir.to_path_buf())),
        Err(_) => rebuild_index(&path, false),
    }
}

/// Sets the damaged index aside and opens an empty one.
fn rebuild_index(path: &Path, repaired: bool) -> Result<OpenedIndex, StoreError> {
    set_aside(path)?;
    Ok(OpenedIndex {
        index: Index::open(path)?,
        acked: None,
        rebuilt: true,
        repaired,
    })
}

/// Removes the file at `path`; a file that is not there is no error.
fn remove_if_present(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Moves `path` to the first free `<path>.damaged-<n>`, never overwriting an existing file.
fn set_aside(path: &Path) -> Result<(), StoreError> {
    for n in 0u32.. {
        let mut name = path.file_name().expect("index file name").to_os_string();
        name.push(format!(".damaged-{n}"));
        match std::fs::hard_link(path, path.with_file_name(name)) {
            Ok(()) => {
                std::fs::remove_file(path)?;
                File::open(path.parent().expect("store dir"))?.sync_all()?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("u32 suffixes exhausted")
}

/// The error for a failed `try_lock` on the store's lock file: only `WouldBlock` (another process
/// holds the lock) is `Locked`; anything else (e.g. a file system without locking) is I/O.
pub fn lock_error(dir: &Path, e: std::fs::TryLockError) -> StoreError {
    match e {
        std::fs::TryLockError::WouldBlock => StoreError::Locked(dir.to_path_buf()),
        std::fs::TryLockError::Error(e) => StoreError::Io(std::io::Error::new(
            e.kind(),
            format!("cannot lock {}: {e}", dir.join("lock").display()),
        )),
    }
}

/// A store's lock file, exclusively locked. Dropping it releases the lock explicitly: the lock
/// belongs to the open file description, which a child process spawned by another thread shares
/// until it execs, so leaving the release to the close would keep the store locked after it is
/// closed, and a reopen in this process would fail as `Locked`.
pub(crate) struct HeldLock(File);

impl HeldLock {
    /// Takes `file`'s exclusive lock without waiting; another holder is `Locked` (lock1).
    pub(crate) fn take(file: File, dir: &Path) -> Result<HeldLock, StoreError> {
        file.try_lock().map_err(|e| lock_error(dir, e))?;
        Ok(HeldLock(file))
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Test-only fault injection (see `fail_next_index_apply`).
#[derive(Default)]
struct Faults {
    index_apply: bool,
    /// The index update of the write after this many more successful writes fails.
    index_apply_after: Option<u32>,
    rollback: bool,
    checkpoint: bool,
    /// The next replay fails before it indexes this frame (counting from 0).
    replay_after: Option<u64>,
}

/// Frames for one `write`: `(kind, payload)` pairs, in write order.
pub(crate) type Frames = Vec<(u8, Vec<u8>)>;

pub(crate) fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).expect("postcard encoding of an in-memory value cannot fail")
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    postcard::from_bytes(bytes).map_err(|e| StoreError::Decode(e.to_string()))
}

/// The single code path that turns a log frame into index entries, used both live and when rebuilding.
fn entries_for_frame(
    kind: u8,
    hash: Hash,
    loc: Location,
    payload: &[u8],
) -> Result<Vec<IndexEntry>, StoreError> {
    if kind == kinds::BATCH_COMMIT {
        // a marker is neither an object nor a dedupe candidate
        return Ok(vec![]);
    }
    let mut out = vec![IndexEntry::Object { kind, hash, loc }];
    match kind {
        kinds::CHANGE => {
            let change: Change = decode(payload)?;
            for op in &change.ops {
                match op {
                    Op::Create { block } | Op::Update { block, .. } => {
                        out.push(IndexEntry::Producer {
                            version: block.version_hash(),
                            change: hash,
                        });
                    }
                    Op::Delete { id, .. } => out.push(IndexEntry::Deleted {
                        block: id.clone(),
                        change: hash,
                    }),
                }
            }
        }
        kinds::REF_UPDATE => {
            let r: RefUpdate = decode(payload)?;
            out.push(IndexEntry::Ref(r.name, r.value));
        }
        kinds::VIEW_OP => {
            let op: ViewOp = decode(payload)?;
            out.push(IndexEntry::ViewOp {
                view: op.view,
                op: hash,
            });
        }
        kinds::LANDING => {
            let rec: LandingRecord = decode(payload)?;
            out.extend(rec.changes.into_iter().map(|change| IndexEntry::Landed {
                change,
                landing: hash,
            }));
        }
        _ => {}
    }
    Ok(out)
}

impl Drop for Store {
    /// Closes the store as `close` does. A failure leaves the sentinel, so the next open catches
    /// up; it cannot be returned from here, so it is written to stderr.
    fn drop(&mut self) {
        use std::io::Write as _;
        if let Err(e) = self.close_once() {
            // a write whose error is dropped, not the print macro: that panics when stderr is
            // closed, and a panic in Drop during an unwind aborts the process
            let _ = writeln!(
                std::io::stderr(),
                "rdy: the store at {} did not close cleanly ({e}); the next open catches its index up from the log",
                self.dir.display()
            );
        }
    }
}

/// Frames a catch-up applies to the index in one commit.
pub const CATCH_UP_CHUNK: usize = 1000;

/// What a catch-up replayed: frames read and index commits made.
#[derive(Default)]
struct CatchUp {
    frames: u64,
    commits: u32,
}

/// Replays the frames of `log` from `from` to its end into `index`, applying their entries every
/// `CATCH_UP_CHUNK` frames, so it never holds more than one chunk. The commits are not durable.
/// `fail_after` is a test fault: the replay fails before it indexes that frame (counting from 0).
fn replay_into(
    log: &SegmentLog,
    index: &Index,
    from: Location,
    fail_after: Option<u64>,
) -> Result<CatchUp, StoreError> {
    let end = log.end();
    let mut done = CatchUp::default();
    let mut entries = vec![];
    let mut in_chunk = 0usize;
    log.for_each_from(from, |loc, kind, hash, payload, next| {
        if fail_after == Some(done.frames) {
            return Err(StoreError::Db("injected replay failure".into()));
        }
        entries.extend(entries_for_frame(kind, hash, loc, payload)?);
        done.frames += 1;
        in_chunk += 1;
        if in_chunk == CATCH_UP_CHUNK {
            index.apply(&entries, next)?;
            done.commits += 1;
            entries.clear();
            in_chunk = 0;
        }
        Ok::<(), StoreError>(())
    })?;
    if in_chunk > 0 || done.commits == 0 {
        index.apply(&entries, end)?;
        done.commits += 1;
    }
    Ok(done)
}

/// Frames (data frames and batch markers) since the last checkpoint that make the next write
/// checkpoint.
pub const CHECKPOINT_FRAMES: u64 = 1000;
/// How long a store with index state not yet checkpointed sits without a write before
/// `checkpoint_if_idle` flushes it.
pub const CHECKPOINT_IDLE: Duration = Duration::from_secs(2);

impl Store {
    /// Creates a store, as `rdy init` does: in a directory that does not exist or is wholly
    /// empty, `FORMAT` is written first (the claim), then the log gets its format ref. A
    /// directory holding anything is never claimed: it is opened as `open` does, so a current-format
    /// store opens (an init retried after a crash) and anything else is refused.
    pub fn create(dir: &Path) -> Result<Store, StoreError> {
        let mut synced = match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_dir() => {
                if std::fs::read_dir(dir)?.next().is_some() {
                    return Store::open(dir);
                }
                vec![]
            }
            Ok(_) => return Store::open(dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // dur1: each directory created here is fsynced into its parent
                fsutil::create_dir_durable(dir)?
            }
            Err(e) => return Err(e.into()),
        };
        format::write_format_file(dir, Format::CURRENT)?;
        synced.push(dir.to_path_buf());
        Store::open_current_format(dir, synced)
    }

    /// Opens an existing store. Opening never creates, upgrades or claims a store (fmt3): a path
    /// that is not a directory with a `FORMAT` file naming the current store format is refused before
    /// anything in it is created, locked, opened or written.
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        Store::check_existing(dir)?;
        Store::open_current_format(dir, vec![])
    }

    /// The checks [`Store::open`] makes before it creates, locks, opens or writes anything: `dir`
    /// is a directory with a `FORMAT` file naming the current store format. Reads only.
    pub fn check_existing(dir: &Path) -> Result<(), StoreError> {
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err(format::other_build(dir, "is not a directory")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format::no_store(dir));
            }
            Err(e) => return Err(e.into()),
        }
        match format::read_format_file(dir)? {
            Some(f) => format::check_format(f, dir),
            None => Err(format::other_build(dir, "has no FORMAT file")),
        }
    }

    /// Opens a store whose `FORMAT` names the current store format (`STORE_FORMAT`).
    fn open_current_format(dir: &Path, mut synced: Vec<PathBuf>) -> Result<Store, StoreError> {
        let segments = dir.join("segments");
        // dur1: each directory created here is fsynced into its parent before any write is acked
        synced.extend(fsutil::create_dir_durable(&segments)?);
        let fresh = !dir.join("index.redb").exists() || !dir.join("lock").exists();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        let lock = HeldLock::take(lock, dir)?; // lock1
        // A rebuild killed part way leaves its temp index (up to the size of the whole index).
        // The lock is held, so no rebuild is running: remove it. This is best effort: a temp
        // that cannot be removed costs only disk space, and the next reindex tries again.
        let _ = remove_if_present(&dir.join(INDEX_TMP));
        // the sentinel of a process that died with the store open
        let unclean = std::fs::symlink_metadata(dir.join(INDEX_OPEN_SENTINEL)).is_ok();
        // seg1: a gap in the segment numbers means acknowledged frames are missing
        if let Some((missing, later)) = find_gap(&segments)? {
            return Err(StoreError::MissingFrames {
                segment: seg_path(&segments, missing),
                detail: format!(
                    "{} exists but {} does not",
                    seg_path(&segments, later).display(),
                    seg_path(&segments, missing).display()
                ),
            });
        }
        // seg1: so does a sealed segment that does not end at a batch marker
        if let Some(seg) = find_unsealed(&segments)? {
            return Err(StoreError::MissingFrames {
                segment: seg_path(&segments, seg),
                detail: format!(
                    "{} does not end at a batch marker, but {} exists",
                    seg_path(&segments, seg).display(),
                    seg_path(&segments, seg + 1).display()
                ),
            });
        }
        // the index opens first: its indexed_upto is the acknowledged end of the log, which
        // tells the log which torn frames are an unacknowledged tail
        let OpenedIndex {
            index,
            acked,
            rebuilt: index_rebuilt,
            repaired: redb_repaired,
        } = open_index(dir, unclean)?;
        if fresh {
            // dur1: the new lock and index files' entries in the store directory
            fsutil::sync_dir(dir)?;
            synced.push(dir.to_path_buf());
        }
        // The log opens by the marker rule: the acknowledged boundary is the last valid batch
        // marker. The index's durable end is only the floor under which damage is refused, never
        // the boundary, because it lags the log.
        let (log, quarantined) = SegmentLog::open_marked(&segments, SEGMENT_MAX, acked)?;
        let report = OpenReport {
            quarantined,
            unclean_shutdown: unclean,
            redb_repaired,
            index_rebuilt,
            ..OpenReport::default()
        };
        let reader = StoreReader {
            index: Arc::new(index),
            shared: Arc::new(ReaderShared {
                dir: dir.to_path_buf(),
                segments: SegmentReader::open(&segments),
                sets: Mutex::new(HashMap::new()),
                frames_read: AtomicU64::new(0),
            }),
            tracking: Arc::default(),
        };
        let mut store = Store {
            dir: dir.to_path_buf(),
            log,
            reader: Some(reader),
            _lock: lock,
            faults: Faults::default(),
            poisoned: false,
            synced,
            report,
            opened: false,
            pending: 0,
            last_write: Instant::now(),
            checkpoint_frames: CHECKPOINT_FRAMES,
            checkpoints: 0,
            checkpoint_error: None,
            writes: 0,
            last_batch: vec![],
            last_batch_end: ORIGIN,
        };
        let (upto, end) = (store.index().indexed_upto()?, store.log.end());
        if upto > end {
            // seg1: the index acknowledged frames the log no longer holds. Rebuilding the index
            // from this log would silently roll canon back (spec §5.10: nothing is deleted).
            let missing = if upto.segment > end.segment {
                end.segment + 1
            } else {
                end.segment
            };
            return Err(StoreError::MissingFrames {
                segment: seg_path(&segments, missing),
                detail: format!(
                    "the index acknowledges writes up to segment {} offset {}, but the log ends at segment {} offset {}",
                    upto.segment, upto.offset, end.segment, end.offset
                ),
            });
        }
        // FORMAT says 3; a log that says otherwise is refused before the index moves
        if let Some(n) = store.format_ref()? {
            format::check_format(Format { store_format: n }, dir)?;
        }
        let caught_up = store.catch_up()?;
        store.report.caught_up_frames = caught_up.frames;
        store.report.catch_up_commits = caught_up.commits;
        store.ensure_format()?;
        // Losing this file to a power loss only costs a needless rebuild, so it is not synced.
        // It is only ever created, never opened: an existing one (a kill's, or a link planted
        // there) is left as it is, and nothing is followed or truncated.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(INDEX_OPEN_SENTINEL))
        {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e.into()),
            _ => {}
        }
        // the format batch a new store writes is not a write of the caller's
        store.last_batch.clear();
        store.last_batch_end = ORIGIN;
        store.opened = true;
        Ok(store)
    }

    /// The log's `meta/store_format` ref, as the index holds it.
    fn format_ref(&self) -> Result<Option<u32>, StoreError> {
        self.index()
            .get_ref(format::REF_STORE_FORMAT)?
            .map(|v| {
                format::decode_u32(&v).ok_or_else(|| {
                    StoreError::Decode(format!(
                        "the log's ref {} is not a decimal number",
                        format::REF_STORE_FORMAT
                    ))
                })
            })
            .transpose()
    }

    /// Makes the log hold the store's format, which `FORMAT` (already checked) says. The log's
    /// ref is the truth: any other number there is refused, and so is a log with frames and no
    /// number. An index with no number over a log with frames may have lost it (an old partial
    /// rebuild), so the index is first rebuilt from the log and asked again. A new store (an
    /// empty log; `create` wrote `FORMAT` first) gets the ref as one marked batch, then a
    /// checkpoint. A crash before that leaves `FORMAT` and an empty log, which the next open
    /// finishes.
    fn ensure_format(&mut self) -> Result<(), StoreError> {
        let held_frames = self.log.end()
            != Location {
                segment: 0,
                offset: 0,
            };
        match self.format_ref()? {
            Some(n) => format::check_format(Format { store_format: n }, &self.dir)?,
            None if held_frames => {
                // an index that lost the ref (an old partial rebuild) is rebuilt from the log;
                // a log that really has no format ref is still refused
                self.reindex()?;
                self.report.index_rebuilt = true;
                match self.format_ref()? {
                    Some(n) => format::check_format(Format { store_format: n }, &self.dir)?,
                    None => {
                        return Err(format::other_build(
                            &self.dir,
                            "has a log with no store format",
                        ));
                    }
                }
            }
            None => {
                self.set_refs(vec![(
                    format::REF_STORE_FORMAT.into(),
                    format::encode_u32(Format::CURRENT.store_format),
                )])?;
                // a new store is durable from its first open
                self.checkpoint()?;
            }
        }
        Ok(())
    }

    /// Test hook: the directories `open` fsynced because it created something in them.
    #[doc(hidden)]
    pub fn durable_dirs(&self) -> &[PathBuf] {
        &self.synced
    }

    /// What the open that produced this store did.
    pub fn open_report(&self) -> &OpenReport {
        &self.report
    }

    /// A reader of this store: a clone of its own (rdr1).
    pub fn reader(&self) -> StoreReader {
        StoreReader::clone(self)
    }

    /// The kind and hash of every data frame the last successful `write` appended, after dedupe
    /// and in write order (batch markers excluded); empty right after open and after a write
    /// that appended nothing. A failed write leaves it as it was.
    pub fn last_batch(&self) -> &[(u8, Hash)] {
        &self.last_batch
    }

    /// The end of the log right after the last successful `write`; the origin right after open.
    pub fn last_batch_end(&self) -> Location {
        self.last_batch_end
    }

    /// Drops the store's own reader, and with it the store's `Arc` of the index, keeping what
    /// readers share. The database closes now unless a cloned reader still holds it.
    fn drop_index(&mut self) -> Option<(Arc<ReaderShared>, Arc<Tracking>)> {
        self.reader.take().map(
            |StoreReader {
                 index,
                 shared,
                 tracking,
             }| {
                drop(index);
                (shared, tracking)
            },
        )
    }

    /// Makes `index` the store's index, in a new reader that shares everything else with the old.
    fn set_index(&mut self, (shared, tracking): (Arc<ReaderShared>, Arc<Tracking>), index: Index) {
        self.reader = Some(StoreReader {
            index: Arc::new(index),
            shared,
            tracking,
        });
    }

    /// trk1: the tracking fields the freeze checks of later commits and landings let change on
    /// an accepted or rejected decision (the engine passes its pack's). Readers taken before
    /// keep the fields they were taken with.
    pub fn set_tracking(&mut self, t: Tracking) {
        self.reader
            .as_mut()
            .expect("the store is open until close")
            .tracking = Arc::new(t);
    }

    /// Replays the frames past the index's `indexed_upto` into it, streaming the log and applying
    /// its entries every `CATCH_UP_CHUNK` frames, so it never holds more than one chunk. Ends with
    /// a checkpoint.
    ///
    /// If `indexed_upto` is not a frame start in this log, the index is not trustworthy: it is set
    /// aside and the whole log is replayed into an empty one. Any other failure is returned with
    /// the index in place.
    fn catch_up(&mut self) -> Result<CatchUp, StoreError> {
        let from = self.index().indexed_upto()?;
        let end = self.log.end();
        if from >= end {
            return Ok(CatchUp::default());
        }
        if self.index_point_unusable(from, end)? {
            // close the file before it is renamed
            let shared = self.drop_index().expect("the store is open until close");
            let path = self.dir.join("index.redb");
            set_aside(&path)?;
            self.set_index(shared, Index::open(&path)?);
            self.report.index_rebuilt = true;
            return self.replay_from(Location {
                segment: 0,
                offset: 0,
            });
        }
        self.replay_from(from)
    }

    /// True if the frame at `from` (before the end of the log and of its segment) cannot be read:
    /// `from` is then no frame start, and the index that holds it is what is wrong.
    ///
    /// A `from` past the end of its segment is not that: the index acknowledged frames the log no
    /// longer holds, so it is `MissingFrames` (seg1), and the index stays as it is.
    fn index_point_unusable(&self, from: Location, end: Location) -> Result<bool, StoreError> {
        if (from.segment == 0 && from.offset == 0) || from >= end {
            return Ok(false);
        }
        let len = self.log.segment_len(from.segment)?;
        if from.offset > len {
            return Err(StoreError::MissingFrames {
                segment: seg_path(&self.dir.join("segments"), from.segment),
                detail: format!(
                    "the index acknowledges writes up to segment {} offset {}, but that segment ends at offset {len}",
                    from.segment, from.offset
                ),
            });
        }
        // at the end of its segment, the next one follows
        Ok(from.offset < len && self.log.read_hashed(from).is_err())
    }

    fn replay_from(&mut self, from: Location) -> Result<CatchUp, StoreError> {
        let fail_after = self.faults.replay_after.take();
        let done = replay_into(&self.log, self.index(), from, fail_after)?;
        // the replayed frames are in the log, not yet in the durable index
        self.checkpoint()?;
        Ok(done)
    }

    /// Reads and hash-checks every frame in every segment and checks every batch marker;
    /// returns the number of data frames (markers are not counted).
    pub fn verify(&self) -> Result<usize, StoreError> {
        Ok(self.log.verify_frames()?)
    }

    /// Every table of the index as raw rows (fsck2: a reindex must reproduce them).
    #[doc(hidden)]
    pub fn index_dump_for_tests(&self) -> Result<crate::index::IndexDump, StoreError> {
        self.index().dump()
    }

    /// Writes one raw `producers` row (a test hook that damages the index).
    #[doc(hidden)]
    pub fn put_raw_producer_for_tests(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.index().put_raw_producer_for_tests(key)
    }

    /// Rebuilds the index from the log. The new index is built in `index.redb.tmp` and is renamed
    /// over `index.redb` only when it is complete and durable. A failure leaves the old index in
    /// place as the store's index, and removes the temp file. The store's reader then holds the
    /// new index; a reader cloned before keeps the old one, which stays readable through its open
    /// file until that reader drops (Unix). A temp that a kill leaves behind is removed by the
    /// next open, which holds the lock.
    pub fn reindex(&mut self) -> Result<(), StoreError> {
        let path = self.dir.join("index.redb");
        let tmp = self.dir.join(INDEX_TMP);
        // a leftover temp is an older failed rebuild; `Index::open` would reuse it
        remove_if_present(&tmp)?;
        let fail_after = self.faults.replay_after.take();
        // an error at any step, the open of the temp included, removes the temp
        let built = Index::open(&tmp).and_then(|ix| {
            replay_into(&self.log, &ix, ORIGIN, fail_after)?;
            // durable before it becomes the index
            ix.checkpoint()?;
            Ok(ix)
        });
        let ix = match built {
            Ok(ix) => ix,
            Err(e) => {
                let _ = remove_if_present(&tmp);
                return Err(e);
            }
        };
        if let Err(e) = std::fs::rename(&tmp, &path) {
            drop(ix);
            let _ = remove_if_present(&tmp);
            return Err(e.into());
        }
        // the handle opened on the temp path keeps working after the rename
        let keep = (Arc::clone(&self.shared), Arc::clone(&self.tracking));
        self.set_index(keep, ix);
        self.shared
            .sets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.pending = 0;
        fsutil::sync_dir(&self.dir)?;
        Ok(())
    }

    pub fn write(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        if self.poisoned {
            return Err(StoreError::Io(std::io::Error::other(
                "an earlier failed write could not be rolled back; reopen the store",
            )));
        }
        let start = self.log.end();
        let result = self.append_and_index(items);
        if result.is_ok() {
            self.writes += 1;
        }
        if result.is_err() {
            // nothing past `start` was acknowledged: discard it — including bytes a partial
            // append left past the end and a segment a failed roll left behind — so neither a
            // later write, a verify nor a reindex can see it
            let rollback = if std::mem::take(&mut self.faults.rollback) {
                Err(std::io::Error::other("injected rollback failure"))
            } else {
                self.log.truncate_to(start)
            };
            if rollback.is_err() {
                self.poisoned = true;
            }
        }
        result
    }

    fn append_and_index(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        let mut hashes = Vec::with_capacity(items.len());
        let mut batch = Vec::with_capacity(items.len());
        let mut seen = BTreeSet::new();
        for (kind, payload) in items {
            let hash = Hash::of(&payload);
            hashes.push(hash);
            // kind1: dedupe by (kind, hash); the same bytes under another kind are another object
            if kind != kinds::REF_UPDATE
                && (seen.contains(&(kind, hash)) || self.index().typed(kind, &hash)?.is_some())
            {
                continue;
            }
            seen.insert((kind, hash));
            batch.push((kind, payload));
        }
        if batch.is_empty() {
            self.last_batch.clear();
            self.last_batch_end = self.log.end();
            return Ok(hashes);
        }
        let placed = self.log.append_batch(&batch)?;
        let mut entries = vec![];
        for ((kind, payload), (loc, hash)) in batch.iter().zip(&placed) {
            entries.extend(entries_for_frame(*kind, *hash, *loc, payload)?);
        }
        self.log.crash_point(CrashPoint::BeforeSync);
        self.log.sync()?;
        self.log.crash_point(CrashPoint::AfterSync);
        let after = match self.faults.index_apply_after {
            Some(0) => self.faults.index_apply_after.take().is_some(),
            Some(n) => {
                self.faults.index_apply_after = Some(n - 1);
                false
            }
            None => false,
        };
        if std::mem::take(&mut self.faults.index_apply) || after {
            return Err(StoreError::Db("injected index failure".into()));
        }
        self.index().apply(&entries, self.log.end())?;
        self.log.crash_point(CrashPoint::AfterIndex);
        self.last_batch = batch
            .iter()
            .zip(&placed)
            .map(|((kind, _), (_, hash))| (*kind, *hash))
            .collect();
        self.last_batch_end = self.log.end();
        // The write is acknowledged: it is durable in the log. A checkpoint is only the index
        // catching up, so a failure of one is recorded and retried, never returned.
        self.pending += batch.len() as u64 + 1;
        self.last_write = Instant::now();
        if self.pending >= self.checkpoint_frames
            && let Err(e) = self.checkpoint()
        {
            self.checkpoint_error = Some(e.to_string());
        }
        Ok(hashes)
    }

    /// Makes the index durable up to everything written so far: one durable index commit. Until
    /// then a kill or power loss leaves the durable index behind the log, which the next open
    /// catches up from the log.
    pub fn checkpoint(&mut self) -> Result<(), StoreError> {
        self.log.crash_point(CrashPoint::BeforeCheckpoint);
        if std::mem::take(&mut self.faults.checkpoint) {
            return Err(StoreError::Db("injected checkpoint failure".into()));
        }
        self.index().checkpoint()?;
        self.log.crash_point(CrashPoint::AfterCheckpoint);
        self.pending = 0;
        self.checkpoints += 1;
        self.checkpoint_error = None;
        Ok(())
    }

    /// Checkpoints, and returns true, if frames were written since the last checkpoint and the
    /// last write was `CHECKPOINT_IDLE` or more before `now`; else does nothing and returns false.
    /// A failure is returned and also recorded (see `last_checkpoint_error`) until a checkpoint
    /// succeeds.
    pub fn checkpoint_if_idle(&mut self, now: Instant) -> Result<bool, StoreError> {
        if self.pending == 0 || now.saturating_duration_since(self.last_write) < CHECKPOINT_IDLE {
            return Ok(false);
        }
        if let Err(e) = self.checkpoint() {
            self.checkpoint_error = Some(e.to_string());
            return Err(e);
        }
        Ok(true)
    }

    /// Closes the store cleanly: checkpoints the index, closes it, then removes the `index.open`
    /// sentinel, so the next open sees no kill. If the checkpoint fails, its error is returned and
    /// the sentinel stays: nothing acknowledged is lost (it is in the log), and the next open
    /// catches the index up. Dropping a store does the same and writes a failure to stderr.
    pub fn close(mut self) -> Result<(), StoreError> {
        self.close_once()
    }

    /// The clean close, once: later calls (and the drop after `close`) do nothing. A store whose
    /// open failed was never `opened`, so it leaves a previous kill's sentinel alone.
    fn close_once(&mut self) -> Result<(), StoreError> {
        if !std::mem::take(&mut self.opened) {
            return Ok(());
        }
        self.checkpoint()?;
        // The database is closed before the sentinel goes, so a kill during the close still
        // leaves the sentinel behind.
        drop(self.drop_index());
        match std::fs::remove_file(self.dir.join(INDEX_OPEN_SENTINEL)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Test hook: a write checkpoints once this many frames are pending.
    #[doc(hidden)]
    pub fn set_checkpoint_frames(&mut self, n: u64) {
        self.checkpoint_frames = n;
    }

    /// Test hook: the checkpoints this store has made.
    #[doc(hidden)]
    pub fn checkpoint_count(&self) -> u64 {
        self.checkpoints
    }

    /// Test hook: the frames written since the last checkpoint.
    #[doc(hidden)]
    pub fn pending_checkpoint_frames(&self) -> u64 {
        self.pending
    }

    /// Test hook: the next checkpoint fails.
    #[doc(hidden)]
    pub fn fail_next_checkpoint(&mut self) {
        self.faults.checkpoint = true;
    }

    /// Test hook: why the last checkpoint a write or `checkpoint_if_idle` attempted failed, until
    /// a checkpoint succeeds.
    #[doc(hidden)]
    pub fn last_checkpoint_error(&self) -> Option<&str> {
        self.checkpoint_error.as_deref()
    }

    /// Test hook: a duplicate of the lock file's descriptor, as a child process spawned while the
    /// store is open holds one until it execs.
    #[doc(hidden)]
    pub fn share_lock_descriptor_for_tests(&self) -> std::io::Result<File> {
        self._lock.0.try_clone()
    }

    /// Test hook: the end of the log.
    #[doc(hidden)]
    pub fn log_end(&self) -> Location {
        self.log.end()
    }

    /// Test hook: arms a crash point; the process aborts when a write or checkpoint reaches it.
    #[doc(hidden)]
    pub fn crash_at(&mut self, p: CrashPoint) {
        self.log.crash_at(Some(p));
    }

    /// Test hook: changes the segment size limit.
    #[doc(hidden)]
    pub fn set_segment_max(&mut self, max: u64) {
        self.log.set_max_len(max);
    }

    /// Test hook: the next write's index update fails after its frames are appended.
    #[doc(hidden)]
    pub fn fail_next_index_apply(&mut self) {
        self.faults.index_apply = true;
    }

    /// Test hook: the index update of a later write fails after its frames are appended: the
    /// write after `writes` more successful (non-empty) writes.
    #[doc(hidden)]
    pub fn fail_index_apply_after(&mut self, writes: u32) {
        self.faults.index_apply_after = Some(writes);
    }

    /// Test hook: the next replay (a catch-up or a reindex) fails with "injected replay failure"
    /// before it indexes frame number `frames`, counting from 0.
    #[doc(hidden)]
    pub fn fail_next_replay_after(&mut self, frames: u64) {
        self.faults.replay_after = Some(frames);
    }

    /// Test hook: the next rollback of a failed write fails, poisoning the store.
    #[doc(hidden)]
    pub fn fail_next_rollback(&mut self) {
        self.faults.rollback = true;
    }

    /// Test hook: the next appended frame is written only in part, then the write fails.
    #[doc(hidden)]
    pub fn fail_next_append_after(&mut self, bytes: usize) {
        self.log.fail_next_append_after(bytes);
    }

    pub fn put<T: Serialize>(&mut self, kind: u8, value: &T) -> Result<Hash, StoreError> {
        Ok(self.write(vec![(kind, encode(value))])?[0])
    }

    /// Successful `write` calls since open (atom1: a landing is one).
    #[doc(hidden)]
    pub fn write_count(&self) -> u64 {
        self.writes
    }

    fn ref_items(&self, refs: Vec<(String, Vec<u8>)>) -> Vec<(u8, Vec<u8>)> {
        let at = self.log.end();
        refs.into_iter()
            .enumerate()
            .map(|(i, (name, value))| {
                let at = Location {
                    segment: at.segment,
                    offset: at.offset + i as u64,
                };
                (kinds::REF_UPDATE, encode(&RefUpdate { name, value, at }))
            })
            .collect()
    }

    /// Writes refs in one batch. A `dismiss/` ref is refused: only `put_dismissal` writes one,
    /// and nothing rewrites it (FW-CH7). A `mention/` ref is refused: only `put_mention_review`
    /// writes one, with its review (FW-CV4).
    pub fn set_refs(&mut self, refs: Vec<(String, Vec<u8>)>) -> Result<(), StoreError> {
        if let Some((name, _)) = refs
            .iter()
            .find(|(name, _)| name.starts_with(crate::review::DISMISS_REF))
        {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is a dismissal: only put_dismissal writes one, and nothing rewrites it"
            )));
        }
        if let Some((name, _)) = refs
            .iter()
            .find(|(name, _)| name.starts_with(crate::review::MENTION_REF))
        {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is a mention review: only put_mention_review writes one"
            )));
        }
        let items = self.ref_items(refs);
        self.write(items)?;
        Ok(())
    }

    /// Test hook: `set_refs` without its checks, to forge refs that fsck must catch.
    #[doc(hidden)]
    pub fn set_refs_unchecked_for_tests(
        &mut self,
        refs: Vec<(String, Vec<u8>)>,
    ) -> Result<(), StoreError> {
        let items = self.ref_items(refs);
        self.write(items)?;
        Ok(())
    }

    /// The hash `put_blob` would return for these bytes, without storing anything.
    pub fn blob_hash(bytes: &[u8]) -> Hash {
        let chunks: Vec<Hash> = bytes.chunks(CHUNK).map(Hash::of).collect();
        Hash::of(&encode(&BlobManifest {
            len: bytes.len() as u64,
            chunks,
        }))
    }

    pub fn put_blob(&mut self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let mut items: Vec<(u8, Vec<u8>)> = bytes
            .chunks(CHUNK)
            .map(|c| (kinds::BLOB_CHUNK, c.to_vec()))
            .collect();
        let chunks: Vec<Hash> = items.iter().map(|(_, c)| Hash::of(c)).collect();
        items.push((
            kinds::BLOB_MANIFEST,
            encode(&BlobManifest {
                len: bytes.len() as u64,
                chunks,
            }),
        ));
        Ok(*self.write(items)?.last().expect("manifest hash"))
    }

    pub fn commit_change(
        &mut self,
        mut change: Change,
        refs: Vec<(String, Vec<u8>)>,
    ) -> Result<Hash, StoreError> {
        let (mut items, change_pos) = self.change_frames(&mut change, None, &[])?;
        items.extend(self.ref_items(refs));
        Ok(self.write(items)?[change_pos])
    }
}

impl StoreReader {
    pub fn dir(&self) -> &Path {
        &self.shared.dir
    }

    /// trk1: the tracking fields this reader's freeze checks use (see [`Store::set_tracking`]).
    pub fn tracking(&self) -> &Arc<Tracking> {
        &self.tracking
    }

    pub(crate) fn index(&self) -> &Index {
        &self.index
    }

    /// The cached (head, set) of view `name`, if `included` has read one.
    pub(crate) fn cached_set(&self, name: &str) -> Option<CachedSet> {
        self.shared
            .sets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// Caches `set` as view `name`'s included set at `head`.
    pub(crate) fn cache_set(&self, name: &str, head: Hash, set: Arc<Vec<Hash>>) {
        self.shared
            .sets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), (head, set));
    }

    /// Calls `f(location, kind, hash, payload, next)` for every frame in `[from, upto)`, batch
    /// markers included, continuing through segment rolls. `upto` comes from the writer
    /// (`log_end()` or `last_batch_end()`), so the walk never meets a batch being appended.
    pub fn for_each_frame_from(
        &self,
        from: Location,
        upto: Location,
        f: impl FnMut(Location, u8, Hash, &[u8], Location) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.shared.segments.for_each_from(from, upto, f)
    }

    /// The first object written with hash `h`, whatever its kind, and that kind.
    pub fn get_raw(&self, h: &Hash) -> Result<(u8, Vec<u8>), StoreError> {
        let loc = self.index().object(h)?.ok_or(StoreError::NotFound(*h))?;
        self.read_object(h, loc)
    }

    /// Reads the frame at `loc`, which the index says holds an object with hash `h`.
    fn read_object(&self, h: &Hash, loc: Location) -> Result<(u8, Vec<u8>), StoreError> {
        self.shared.frames_read.fetch_add(1, Ordering::Relaxed);
        let (kind, found, payload) = self.shared.segments.read_hashed(loc)?;
        if found != *h {
            return Err(StoreError::Decode(format!(
                "the index places {h} at {loc:?}, which holds {found}; run rdy reindex"
            )));
        }
        Ok((kind, payload))
    }

    /// The payload of the object of `kind` with hash `h` (kind1).
    pub fn get_kind(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError> {
        let loc = self
            .index()
            .typed(kind, h)?
            .ok_or(StoreError::NotFound(*h))?;
        let (found_kind, payload) = self.read_object(h, loc)?;
        if found_kind != kind {
            return Err(StoreError::Decode(format!(
                "the index places a kind {kind} object {h} at {loc:?}, which holds kind {found_kind}; run rdy reindex"
            )));
        }
        Ok(payload)
    }

    /// The object of `kind` with hash `h`, decoded.
    pub fn get_typed<T: DeserializeOwned>(&self, kind: u8, h: &Hash) -> Result<T, StoreError> {
        decode(&self.get_kind(kind, h)?)
    }

    /// The object of `kind` with hash `h`, decoded; an object of another kind with that hash is
    /// named as what it is (`{h} is not a {what} (object kind {k})`), never decoded as `T`.
    pub(crate) fn get_named<T: DeserializeOwned>(
        &self,
        kind: u8,
        what: &str,
        h: &Hash,
    ) -> Result<T, StoreError> {
        match self.get_typed(kind, h) {
            Err(StoreError::NotFound(_)) if self.has(h)? => {
                let (k, _) = self.get_raw(h)?;
                Err(StoreError::Decode(format!(
                    "{h} is not a {what} (object kind {k})"
                )))
            }
            r => r,
        }
    }

    /// The change with id `h` (repr1).
    pub fn change(&self, h: &Hash) -> Result<Change, StoreError> {
        self.get_named(kinds::CHANGE, "change", h)
    }

    /// The proposal version with id `h` (repr1).
    pub fn proposal(&self, h: &Hash) -> Result<Proposal, StoreError> {
        self.get_named(kinds::PROPOSAL, "proposal", h)
    }

    /// The landing with id `h` (repr1).
    pub fn landing(&self, h: &Hash) -> Result<LandingRecord, StoreError> {
        self.get_named(kinds::LANDING, "landing", h)
    }

    /// Whether an object of `kind` with hash `h` is stored: an index lookup that reads no frame.
    pub fn has_kind(&self, kind: u8, h: &Hash) -> Result<bool, StoreError> {
        Ok(self.index().typed(kind, h)?.is_some())
    }

    /// Object frames read so far by `get_raw` and `get_kind`.
    #[doc(hidden)]
    pub fn frames_read(&self) -> u64 {
        self.shared.frames_read.load(Ordering::Relaxed)
    }

    pub fn get<T: DeserializeOwned>(&self, h: &Hash) -> Result<T, StoreError> {
        decode(&self.get_raw(h)?.1)
    }

    pub fn has(&self, h: &Hash) -> Result<bool, StoreError> {
        Ok(self.index().object(h)?.is_some())
    }

    pub fn get_block(&self, version: &Hash) -> Result<Block, StoreError> {
        self.get(version)
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.index().get_ref(name)
    }

    pub fn refs_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.index().refs_with_prefix(prefix)
    }

    /// How many refs start with `prefix`, without reading their values.
    pub fn count_refs(&self, prefix: &str) -> Result<usize, StoreError> {
        self.index().count_refs(prefix)
    }

    /// Whether `h` names a blob in this store: a manifest-kind object, which only `put_blob`
    /// writes. One index lookup; it reads no frame.
    pub fn has_blob(&self, h: &Hash) -> Result<bool, StoreError> {
        self.has_kind(kinds::BLOB_MANIFEST, h)
    }

    pub fn get_blob(&self, h: &Hash) -> Result<Vec<u8>, StoreError> {
        let mut out = Vec::new();
        self.read_blob(h, &mut |chunk| out.extend_from_slice(chunk))?;
        Ok(out)
    }

    /// Validates the manifest-kind object at `h` against what `put_blob` writes, then hands each
    /// chunk to `sink` in order. Nothing is read before the manifest is known to be canonical.
    fn read_blob(&self, h: &Hash, sink: &mut dyn FnMut(&[u8])) -> Result<(), StoreError> {
        let payload = match self.get_kind(kinds::BLOB_MANIFEST, h) {
            Ok(p) => p,
            // kind1: an object of another kind with this hash is never the blob
            Err(StoreError::NotFound(_)) if self.has(h)? => {
                let (kind, _) = self.get_raw(h)?;
                return Err(StoreError::Decode(format!(
                    "{h} is not a blob (object kind {kind})"
                )));
            }
            Err(e) => return Err(e),
        };
        let m: BlobManifest = decode(&payload)?;
        // postcard ignores trailing bytes; a manifest that is not its own canonical encoding
        // is not what `put_blob` wrote, and would not re-import under its own name
        if encode(&m) != payload {
            return Err(StoreError::Decode(format!(
                "{h} is not a canonical blob manifest"
            )));
        }
        // the chunk count `put_blob` would produce, checked before anything is allocated or read
        if m.chunks.len() as u64 != m.len.div_ceil(CHUNK as u64) {
            return Err(StoreError::Decode(format!(
                "blob {h} lists {} chunks for {} bytes",
                m.chunks.len(),
                m.len
            )));
        }
        let mut left = m.len;
        for c in &m.chunks {
            // the chunk-kind object, or failing that any object with the chunk's hash: a chunk
            // written before kind1 may have deduped into a block or snapshot of the same bytes,
            // and the hash proves the bytes
            let bytes = match self.get_kind(kinds::BLOB_CHUNK, c) {
                Err(StoreError::NotFound(_)) => self.get_raw(c)?.1,
                r => r?,
            };
            // every chunk but the last is a full chunk, as `put_blob` cuts them
            let want = left.min(CHUNK as u64);
            if bytes.len() as u64 != want {
                return Err(StoreError::Decode(format!(
                    "blob {h} has a chunk of {} bytes where {want} are due",
                    bytes.len()
                )));
            }
            left -= want;
            sink(&bytes);
        }
        Ok(())
    }

    /// The change that carried `version` into canon: of its producers, the one whose landing was
    /// written first, or, when none has landed, the one written first. A version two changes
    /// wrote identically has two producers; a side from canon names the one canon got it from.
    pub fn producer(&self, version: &Hash) -> Result<Option<Hash>, StoreError> {
        let all = self.index().producers(version)?;
        if all.len() <= 1 {
            return Ok(all.first().copied());
        }
        let at = |kind: u8, h: &Hash| -> Result<Location, StoreError> {
            self.index().typed(kind, h)?.ok_or(StoreError::NotFound(*h))
        };
        let mut landed: Option<(Location, Hash)> = None;
        for c in &all {
            if let Some(l) = self.index().landing_of(c)? {
                let key = (at(kinds::LANDING, &l)?, *c);
                landed = Some(landed.map_or(key, |k| k.min(key)));
            }
        }
        if let Some((_, c)) = landed {
            return Ok(Some(c));
        }
        let mut first: Option<(Location, Hash)> = None;
        for c in &all {
            let key = (at(kinds::CHANGE, c)?, *c);
            first = Some(first.map_or(key, |k| k.min(key)));
        }
        Ok(first.map(|(_, c)| c))
    }

    /// For each of `blocks` that canon removed at or before landing `head`, the change that
    /// removed it most recently: of the deletes of it that landed by then, those of the latest
    /// landing whose state lacks the block while the state before it held it (of one landing's,
    /// the smallest hash). A delete that landed after the block was already gone removed nothing
    /// and is never the one. None for a block with no such landing, and for all with no `head`.
    /// A merge onto `head`'s state names it as the side of a block that state lacks, and a
    /// re-create committed on that base depends on it, so the two agree.
    pub fn deleters_at<'a>(
        &self,
        blocks: impl IntoIterator<Item = &'a BlockId>,
        head: Option<&Hash>,
    ) -> Result<HashMap<BlockId, Hash>, StoreError> {
        let Some(head) = head else {
            return Ok(HashMap::new());
        };
        let rows = self.index().deleter_rows(blocks)?;
        self.removers(rows, head)
    }

    /// The `deleter` lookup of a merge onto `base`, the state of landing `at`: `deleters_at(at)`
    /// for each of `blocks` (the blocks the merged changes write) that `base` lacks. One index
    /// read when none of them was ever deleted.
    pub fn merge_deleters<'a>(
        &self,
        at: Option<&Hash>,
        base: &StateSnapshot,
        blocks: impl IntoIterator<Item = &'a BlockId>,
    ) -> Result<HashMap<BlockId, Hash>, StoreError> {
        self.deleters_at(
            blocks.into_iter().filter(|b| !base.blocks.contains_key(*b)),
            at,
        )
    }

    /// Whether any of `blocks` has a change that deletes it, landed or not: one index read.
    pub fn ever_deleted<'a>(
        &self,
        blocks: impl IntoIterator<Item = &'a BlockId>,
    ) -> Result<bool, StoreError> {
        Ok(!self.index().deleter_rows(blocks)?.is_empty())
    }

    /// `deleters_at` for one block.
    pub fn deleter_at(
        &self,
        block: &BlockId,
        head: Option<&Hash>,
    ) -> Result<Option<Hash>, StoreError> {
        Ok(self.deleters_at([block], head)?.remove(block))
    }

    /// `deleters_at` over rows already read from the index (`Index::deleter_rows`).
    #[allow(clippy::type_complexity)]
    pub(crate) fn removers(
        &self,
        rows: BTreeMap<BlockId, Vec<(Hash, Option<(Hash, Location)>)>>,
        head: &Hash,
    ) -> Result<HashMap<BlockId, Hash>, StoreError> {
        let mut out = HashMap::new();
        if rows.values().all(|r| r.iter().all(|(_, at)| at.is_none())) {
            return Ok(out);
        }
        let bound = self
            .index()
            .typed(kinds::LANDING, head)?
            .ok_or(StoreError::NotFound(*head))?;
        // the states of the landings read, by landing (None: before any landing)
        let mut states: HashMap<Option<Hash>, StateSnapshot> = HashMap::new();
        for (block, rows) in rows {
            // the landings that carried a delete of `block`, latest first, each with its deletes
            let mut landings: BTreeMap<Reverse<Location>, (Hash, Vec<Hash>)> = BTreeMap::new();
            for (c, at) in rows {
                if let Some((l, loc)) = at
                    && loc <= bound
                {
                    landings
                        .entry(Reverse(loc))
                        .or_insert((l, vec![]))
                        .1
                        .push(c);
                }
            }
            for (l, deletes) in landings.into_values() {
                let rec = self.landing(&l)?;
                let gone = !self
                    .state_cached(&mut states, Some(l))?
                    .blocks
                    .contains_key(&block);
                if gone
                    && self
                        .state_cached(&mut states, rec.prev)?
                        .blocks
                        .contains_key(&block)
                {
                    out.insert(block, *deletes.iter().min().expect("one delete at least"));
                    break;
                }
            }
        }
        Ok(out)
    }

    fn state_cached<'s>(
        &self,
        states: &'s mut HashMap<Option<Hash>, StateSnapshot>,
        landing: Option<Hash>,
    ) -> Result<&'s StateSnapshot, StoreError> {
        if let std::collections::hash_map::Entry::Vacant(e) = states.entry(landing) {
            e.insert(self.state_of(landing)?);
        }
        Ok(&states[&landing])
    }

    /// Of `changes` (producers of one version), the one that last wrote it as of landing `head`:
    /// the one whose landing was written last at or before `head` (of one landing's, the one
    /// written last), or, when none had landed by then (or there is no `head`), the one written
    /// first. A block written back to an earlier text has that text's version again; blame
    /// names the change that wrote it last, where `producer` names the one canon first got it
    /// from.
    pub fn producer_at(
        &self,
        changes: &[Hash],
        head: Option<&Hash>,
    ) -> Result<Option<Hash>, StoreError> {
        if changes.len() <= 1 {
            return Ok(changes.first().copied());
        }
        let at = |kind: u8, h: &Hash| -> Result<Location, StoreError> {
            self.index().typed(kind, h)?.ok_or(StoreError::NotFound(*h))
        };
        if let Some(head) = head {
            let bound = at(kinds::LANDING, head)?;
            let mut landed: Option<(Location, Location, Hash)> = None;
            for c in changes {
                if let Some(l) = self.index().landing_of(c)? {
                    let l = at(kinds::LANDING, &l)?;
                    if l <= bound {
                        let key = (l, at(kinds::CHANGE, c)?, *c);
                        landed = Some(landed.map_or(key, |k| k.max(key)));
                    }
                }
            }
            if let Some((_, _, c)) = landed {
                return Ok(Some(c));
            }
        }
        let mut first: Option<(Location, Hash)> = None;
        for c in changes {
            let key = (at(kinds::CHANGE, c)?, *c);
            first = Some(first.map_or(key, |k| k.min(key)));
        }
        Ok(first.map(|(_, c)| c))
    }

    /// Of `changes`, the one written last.
    pub fn written_last(&self, changes: &[Hash]) -> Result<Option<Hash>, StoreError> {
        if changes.len() <= 1 {
            return Ok(changes.first().copied());
        }
        let mut last: Option<(Location, Hash)> = None;
        for c in changes {
            let loc = self
                .index()
                .typed(kinds::CHANGE, c)?
                .ok_or(StoreError::NotFound(*c))?;
            let key = (loc, *c);
            last = Some(last.map_or(key, |k| k.max(key)));
        }
        Ok(last.map(|(_, c)| c))
    }

    /// The changes with an op that produces `version`, ascending.
    pub fn producers(&self, version: &Hash) -> Result<Vec<Hash>, StoreError> {
        self.index().producers(version)
    }

    /// The changes with an op that deletes `block`, ascending.
    pub fn deleters(&self, block: &BlockId) -> Result<Vec<Hash>, StoreError> {
        self.index().deleters(block)
    }
}

impl Store {
    /// How a change is laid out in a write, the one definition every change-writing path uses:
    /// sets `change.deps` (see `change_deps`; `view` is the view it is committed to, if any), then
    /// returns the `BLOCK` frames of its creates and updates followed by
    /// the `CHANGE` frame, and the position of that `CHANGE` frame. The change's id is
    /// `Hash::of(&items[change_pos].1)`.
    pub(crate) fn change_frames(
        &self,
        change: &mut Change,
        view: Option<&str>,
        conflicts: &[Conflict],
    ) -> Result<(Frames, usize), StoreError> {
        change.deps = self.change_deps_resolving(change, view, conflicts)?;
        let mut items = vec![];
        for op in &change.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                items.push((kinds::BLOCK, encode(block)));
            }
        }
        items.push((kinds::CHANGE, encode(change)));
        let change_pos = items.len() - 1;
        Ok((items, change_pos))
    }

    pub(crate) fn ref_items_pub(&self, refs: Vec<(String, Vec<u8>)>) -> Vec<(u8, Vec<u8>)> {
        self.ref_items(refs)
    }
}

impl StoreReader {
    /// The deps the store gives `change` when it commits it to `view` (or, with no view, through
    /// `commit_change`), sorted ascending and deduplicated:
    /// - for each op's base, its producers that `view` includes; when it includes none (the base
    ///   came from canon), or there is no view, `producer(base)`. A version two views wrote
    ///   identically has a producer in each, and a dep must name the writer's own;
    /// - for each id one of its ops creates: when committed to `view`, every change `view`
    ///   includes that deletes it (so a delete and a re-create under one id merge in that
    ///   order); and the delete that removed it from canon as of the landing the writer read
    ///   (`effective_base`, canon's head with no view; see `deleters_at`), as the writer saw the
    ///   id deleted there. A re-create that does not depend on the delete that removed the id
    ///   from its merge base conflicts with it in the merge.
    pub fn change_deps(
        &self,
        change: &Change,
        view: Option<&str>,
    ) -> Result<Vec<Hash>, StoreError> {
        self.change_deps_resolving(change, view, &[])
    }

    /// `change_deps`, plus, for each op whose block has a conflict in `conflicts`, the change of
    /// every side that has one: a write under a recorded conflict depends on every side, so the
    /// merge reads it as the conflict's resolution (res1).
    pub fn change_deps_resolving(
        &self,
        change: &Change,
        view: Option<&str>,
        conflicts: &[Conflict],
    ) -> Result<Vec<Hash>, StoreError> {
        let mut deps = BTreeSet::new();
        // `view`'s included set, read once and only when a dep needs it
        let mut included: Option<HashSet<Hash>> = None;
        for op in &change.ops {
            match op {
                Op::Create { .. } => {}
                Op::Update { base, .. } | Op::Delete { base, .. } => {
                    let producers = self.index().producers(base)?;
                    if producers.is_empty() {
                        return Err(StoreError::NotFound(*base));
                    }
                    if producers.len() > 1
                        && let Some(view) = view
                    {
                        let included = self.included_set(&mut included, view)?;
                        let own: Vec<Hash> = producers
                            .into_iter()
                            .filter(|p| included.contains(p))
                            .collect();
                        if !own.is_empty() {
                            deps.extend(own);
                            continue;
                        }
                    }
                    deps.insert(self.producer(base)?.ok_or(StoreError::NotFound(*base))?);
                }
            }
        }
        // the creates: one index read for all their ids, and more only for an id once deleted
        let rows = self
            .index()
            .deleter_rows(change.ops.iter().filter_map(|op| match op {
                Op::Create { block } => Some(&block.id),
                _ => None,
            }))?;
        if !rows.is_empty() {
            let base = match view {
                Some(view) => {
                    let included = self.included_set(&mut included, view)?;
                    deps.extend(
                        rows.values()
                            .flatten()
                            .map(|(d, _)| *d)
                            .filter(|d| included.contains(d)),
                    );
                    self.effective_base(view)?
                }
                None => self.canon_head()?,
            };
            if let Some(base) = base {
                deps.extend(self.removers(rows, &base)?.into_values());
            }
        }
        for op in &change.ops {
            if let Some(c) = conflicts.iter().find(|c| c.block == *op.block_id()) {
                deps.extend(c.sides.iter().filter_map(|s| s.change));
            }
        }
        Ok(deps.into_iter().collect())
    }

    /// The landing view `view` reads from: canon's head for a view with no head or one that
    /// floats (unpinned, nothing included; its next commit catches it up to canon), else its
    /// stored base.
    pub fn effective_base(&self, view: &str) -> Result<Option<Hash>, StoreError> {
        match self.view_head(view)? {
            Some(h) if h.pinned || h.len > 0 => Ok(h.base),
            _ => self.canon_head(),
        }
    }

    fn included_set<'a>(
        &self,
        slot: &'a mut Option<HashSet<Hash>>,
        view: &str,
    ) -> Result<&'a HashSet<Hash>, StoreError> {
        if slot.is_none() {
            *slot = Some(self.included(view)?.iter().copied().collect());
        }
        Ok(slot.as_ref().expect("set above"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn msw4_store_drop_never_uses_eprintln() {
        let src = include_str!("store.rs");
        let start = src.find("impl Drop for Store").unwrap();
        let end = start + src[start..].find("\n}\n").unwrap();
        assert!(
            !src[start..end].contains("eprintln!"),
            "{}",
            &src[start..end]
        );
    }
}
