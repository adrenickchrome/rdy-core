use crate::error::StoreError;
use crate::fileid::FileId;
use crate::format::{self, StoreFeatures};
use crate::fsutil;
use crate::holder::{Creation, FirstBatch, RefRules, RefWrite, StoreMode, Walk};
use crate::index::{Index, IndexEntry};
use crate::kinds::{self, BlobManifest, RefUpdate};
use crate::merge::Conflict;
use crate::plan::{Held, NoneHeld, Resolver};
use crate::redaction_io::{RedactionCounts, VerifyReport};
use crate::segment::{
    CrashPoint, HEADER, Location, SegmentLog, SegmentReader, find_gap, find_unsealed, seg_path,
};
use crate::transition::Transitions;
use crate::view::ViewOp;
use rdy_model::{
    Block, BlockId, Change, Hash, LandingRecord, Op, Proposal, RepoId, StateSnapshot, Tracking,
};
use serde::{Serialize, de::DeserializeOwned};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// What a blob's metadata permits ([`StoreReader::blob_state`]), read without its chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobState {
    /// The manifest is canonical and no removal masks it or a chunk. A byte read can still fail
    /// on a damaged or absent chunk.
    Readable,
    /// A removal masks the manifest or a chunk: the request of the earliest authorizing apply.
    Removed(Hash),
    /// No manifest-kind object has this hash.
    Missing,
}

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
///
/// With held batches (plan M2e2, D-Z0EMP), a write indexes its frames before the batch's marker
/// and sync, so a clone on another thread can see an open batch that a failed seal or a kill
/// then removes. Index reads off the writer thread must not drive decisions on data an open
/// batch wrote: no refusal, no logged record, no notification. The writer decides; other threads
/// read the state it published after the seal.
#[derive(Clone)]
pub struct StoreReader {
    index: Arc<Index>,
    shared: Arc<ReaderShared>,
    /// trk1: the project pack's tracking fields, which the freeze checks let change on a frozen
    /// decision. Empty until the engine calls [`Store::set_tracking`].
    tracking: Arc<Tracking>,
    /// The redaction epoch of the reader family when this reader was taken (a clone keeps it).
    epoch: u64,
    /// The masks of recorded requests this reader reads as applied (`redaction_preview_reader`);
    /// None for every other reader.
    preview: Option<Arc<crate::redaction::Preview>>,
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
    /// Ref entries read by `refs_with_prefix` and `open_proposal_refs` (a test hook).
    refs_read: AtomicU64,
    /// Log frames read by `visit_carried` (a test hook).
    carried_frames_read: AtomicU64,
    /// The redaction epoch of this store family: zero at open; it advances once per newly synced
    /// apply batch, before any payload is erased. A reader whose captured epoch is older reads a
    /// state that predates content removal.
    epoch: Arc<AtomicU64>,
    /// The store's poisoned state, as its readers see it (`Store::poisoned`).
    poisoned: AtomicBool,
    /// Set when a removal failed after its apply was synced (E_REDACTION_PENDING): no reader of
    /// this family serves content again; a reopen finishes the removal.
    pending: AtomicBool,
    /// What the open permits: a read-only store refuses every write (E_STORE_READ_ONLY).
    access: Access,
    /// A read-only store's lock and private index directory, held until the last reader drops.
    /// Declared last: the readers' index is closed before the directory goes.
    _hold: Option<ReadOnlyHold>,
}

/// What a store's open permits ([`Store::access`]). A read-only store keeps its first unknown
/// read-only feature by name and the build that feature names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Access {
    ReadWrite,
    ReadOnly {
        feature: String,
        required_rdy: String,
    },
}

/// What every reader of a read-only store holds: the store's lock, so no process writes or
/// removes content beneath a read, and the private directory of the index built from the log,
/// removed when the last reader drops.
struct ReadOnlyHold {
    _lock: StoreLock,
    dir: tempfile::TempDir,
}

/// How an open treats a store that needs an upgrade (D-WWAK7). Neither policy stops a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenPolicy {
    /// Plan the upgrade, take the store lock, plan again and apply it, then open the upgraded
    /// store. A sealed upgrade whose completion is still to come opens with that completion
    /// pending: the engine runs its full fsck, then publishes `FORMAT`.
    AutoUpgrade,
    /// Refuse a store with planned steps or a pending completion (E_STORE_FORMAT, `the store
    /// needs an upgrade before it can open`), before any backup, append or mirror publication.
    RequireCurrent,
}

pub struct Store {
    dir: PathBuf,
    report: OpenReport,
    log: SegmentLog,
    /// The store's own reader, which holds the only store-side `Arc` of the index. None only once
    /// the clean close (`close` or `Drop`) has dropped it.
    reader: Option<StoreReader>,
    /// Declared after `reader`, so the database is closed before the lock is released (by this
    /// store; a caller that gave the lock to `open_locked` may hold it longer).
    _lock: StoreLock,
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
    /// Held batches are on (plan M2e2, D-Z0EMP): a `write` appends and indexes its frames but
    /// writes no marker and makes no sync, and `seal` ends the batch. Off after open.
    held: bool,
    /// The held batch not yet sealed, if any.
    open: Option<OpenBatch>,
    /// Set by a failed seal: the index holds entries of frames the log no longer holds, so no
    /// checkpoint may make it durable, and the close removes it. The next open rebuilds it from
    /// the log, and a reindex rebuilds it at once.
    stale_index: bool,
    /// What the log's first sealed batch set (store format 15): the store's mode, which the ref
    /// rules of every write and replay follow, and where that batch ends.
    creation: Creation,
    /// The features the log's latest `meta/store_features` ref holds (sealed batches only).
    features: StoreFeatures,
    /// The features of the open held batch, when one of its writes activated a feature: they
    /// become `features` when the batch seals.
    held_features: Option<StoreFeatures>,
    /// Opened by the upgrade transaction ([`Store::open_unupgraded_locked`]): a `FORMAT` that
    /// still mirrors the manifest before the log's last upgrade receipt is accepted as it is.
    accept_pending: bool,
    /// A sealed upgrade whose `FORMAT` mirror is still the old one: the engine's full fsck and
    /// `crate::upgrade::publish_checked` are still to come, and every write is refused
    /// (`UpgradeRequired`) until [`Store::refresh_upgrade_pending`] finds the mirror published.
    completion_pending: bool,
    /// The last unheld batch's marker is synced and its index update has not finished: set by
    /// `append_and_index` between the two. The upgrade's commit (`commit_upgrade_refs`) reads it
    /// to keep a batch whose index update failed after its sync.
    synced_unindexed: bool,
}

/// A held batch not yet sealed: where its first data frame is, and the kind and hash of each of
/// its frames, in order.
struct OpenBatch {
    start: Location,
    frames: Vec<(u8, Hash)>,
}

/// The refusal of a step that would make the index durable, or read the log as a whole, while a
/// held batch is open.
pub(crate) fn batch_open(what: &str) -> StoreError {
    StoreError::Refused(format!("a held batch is open: seal it {what}"))
}

/// The error of every write after a failed write that could not be rolled back.
fn poisoned_error() -> StoreError {
    StoreError::Io(std::io::Error::other(
        "an earlier failed write could not be rolled back; reopen the store",
    ))
}

/// The holes of the active spans `spans` (from the index), for a hole-aware walk.
pub(crate) fn holes_of(
    spans: &BTreeMap<Location, crate::index::ActiveSpan>,
) -> crate::segment::Holes {
    spans
        .iter()
        .map(|(loc, s)| {
            (
                *loc,
                crate::segment::Hole {
                    kind: s.span.kind,
                    hash: s.span.hash,
                    len: s.span.len,
                },
            )
        })
        .collect()
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

/// The lock of a store directory, held (lock1). An open store holds one; a caller can take one
/// first and give it to [`Store::open_locked`], so no other process opens the store between
/// the two (a load holds it from before the rename of its store until the load ends). The lock
/// is released when the last holder drops it.
#[derive(Clone)]
pub struct StoreLock(Arc<HeldLock>);

impl StoreLock {
    /// Creates `dir/lock` if missing and takes its lock without waiting; another holder is
    /// `Locked`.
    pub fn take(dir: &Path) -> Result<StoreLock, StoreError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        Ok(StoreLock(Arc::new(HeldLock::take(file, dir)?)))
    }

    /// Takes the exclusive lock of the existing `dir/lock` without waiting, as [`StoreLock::take`]
    /// does, and never creates the file: a read-only open writes nothing in its source store. A
    /// missing lock file is `ReadOnlyNoLock`; another holder is `Locked`.
    pub fn take_existing(dir: &Path) -> Result<StoreLock, StoreError> {
        let file = match OpenOptions::new().read(true).open(dir.join("lock")) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::ReadOnlyNoLock);
            }
            Err(e) => return Err(e.into()),
        };
        Ok(StoreLock(Arc::new(HeldLock::take(file, dir)?)))
    }

    /// Whether this is the lock of the store at `dir`: the held file is the file at `dir/lock`.
    /// A store directory renamed after the lock was taken keeps its lock.
    pub(crate) fn guards(&self, dir: &Path) -> bool {
        matches!(
            (FileId::of_file(&self.0.0), FileId::lstat(&dir.join("lock"))),
            (Ok(a), Ok(b)) if a == b
        )
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
    /// Called by a take before it reads and appends each new record, with the records appended
    /// so far (`on_take_append_for_tests`).
    take_append: Option<Box<dyn FnMut(u64) + Send>>,
}

/// Frames for one `write`: `(kind, payload)` pairs, in write order.
pub(crate) type Frames = Vec<(u8, Vec<u8>)>;

/// What [`Store::first_use`] made of a batch: its items, the merged manifest it activates, if
/// any, and whether `FORMAT` was rewritten for it.
type FirstUse = (Frames, Option<StoreFeatures>, bool);

pub(crate) fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).expect("postcard encoding of an in-memory value cannot fail")
}

/// Checks one chunk of blob `h` against the length `put_blob` cuts there, with `left` bytes of
/// the blob still due: every chunk but the last is a full chunk. Returns the bytes due after it;
/// any other length is `Decode` (`blob {h} has a chunk of {len} bytes where {want} are due`).
/// The one rule a blob read, the fsck and a holder's take keep.
pub fn check_chunk_len(h: &Hash, left: u64, len: u64) -> Result<u64, StoreError> {
    let want = left.min(CHUNK as u64);
    if len != want {
        return Err(StoreError::Decode(format!(
            "blob {h} has a chunk of {len} bytes where {want} are due"
        )));
    }
    Ok(left - want)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    postcard::from_bytes(bytes).map_err(|e| StoreError::Decode(e.to_string()))
}

/// The single code path that turns a log frame into index entries, used both live and when rebuilding.
/// `r` resolves what a plan entry reads of earlier objects: the store and the frames of the pass
/// before this one, which it then notes.
fn entries_for_frame(
    kind: u8,
    hash: Hash,
    loc: Location,
    payload: &[u8],
    r: &mut Resolver<'_>,
) -> Result<Vec<IndexEntry>, StoreError> {
    let out = frame_entries(kind, hash, loc, payload, r)?;
    r.note(crate::codec::logical_kind(kind), hash, loc)?;
    Ok(out)
}

/// `entries_for_frame` before the frame is noted.
fn frame_entries(
    kind: u8,
    hash: Hash,
    loc: Location,
    payload: &[u8],
    r: &Resolver<'_>,
) -> Result<Vec<IndexEntry>, StoreError> {
    if kind == kinds::BATCH_COMMIT {
        // a marker is neither an object nor a dedupe candidate
        return Ok(vec![]);
    }
    // the logical kind and the body of the frame's encoding (`crate::codec`): an index, a
    // dependency and a redaction identity know objects by their logical kind
    let o = crate::codec::split(kind, payload).map_err(|e| StoreError::Decode(e.to_string()))?;
    let (kind, payload) = (o.kind, o.body);
    let mut out = vec![IndexEntry::Object { kind, hash, loc }];
    match kind {
        kinds::CHANGE => {
            let change: Change = crate::codec::decode_split(&o)?;
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
            let rec: LandingRecord = crate::codec::decode_split(&o)?;
            out.extend(rec.changes.into_iter().map(|change| IndexEntry::Landed {
                change,
                landing: hash,
            }));
        }
        kinds::SIGNATURE => {
            // the bounded decoder only: a record that does not decode is damage
            let g = crate::signature::decode_stored(&hash, payload)?;
            out.push(IndexEntry::Signature {
                subject: g.subject,
                signature: hash,
            });
        }
        kinds::REDACTION => out.push(crate::redaction::request_entry(hash, payload)?),
        kinds::REDACTION_APPLY => out.push(crate::redaction::apply_entry(hash, loc, payload)?),
        kinds::TRANSITION => {
            let rec: Transitions = decode(payload)?;
            out.extend(
                rec.resolved
                    .iter()
                    .map(|t| IndexEntry::TensionShut { id: t.tension }),
            );
            out.extend(rec.opened.iter().map(|t| IndexEntry::TensionOpen {
                id: t.tension,
                record: hash,
            }));
            out.extend(
                rec.gone
                    .iter()
                    .map(|m| IndexEntry::SuspectShut { key: m.key() }),
            );
            out.extend(rec.new.iter().map(|m| IndexEntry::SuspectOpen {
                key: m.key(),
                record: hash,
            }));
            out.extend(crate::plan::transition_entries(&rec, loc));
        }
        kinds::ANSWER => out.extend(crate::plan::answer_entries(hash, loc, payload, r)?),
        kinds::PLAN => out.extend(crate::plan::plan_entries(hash, loc, payload, r)?),
        _ => {}
    }
    Ok(out)
}

/// What a replay's plan entries read of earlier objects: the index being built and the log, with
/// its authorized holes (a removed block version's id is its residue's).
struct ReplayHeld<'a> {
    index: &'a Index,
    log: &'a SegmentLog,
    auth: &'a crate::redaction_io::Authorization,
}

impl Held for ReplayHeld<'_> {
    fn block_id(&self, v: &Hash) -> Result<Option<BlockId>, StoreError> {
        match self.index.typed(kinds::BLOCK, v)? {
            Some(loc) => self.block_id_at(loc, v),
            None => Ok(None),
        }
    }

    fn block_id_at(&self, loc: Location, v: &Hash) -> Result<Option<BlockId>, StoreError> {
        if let Some(span) = self.auth.spans.get(&loc) {
            return Ok(
                match span.residue.and_then(|r| self.auth.residues.get(&r)) {
                    Some(rdy_model::redaction::RedactionResidue::Block(b)) => Some(b.id.clone()),
                    _ => None,
                },
            );
        }
        Ok(match self.log.read_hashed(loc) {
            Ok((raw, found, payload))
                if found == *v && crate::codec::logical_kind(raw) == kinds::BLOCK =>
            {
                crate::codec::decode_record::<Block>(raw, &payload)
                    .ok()
                    .map(|b| b.id)
            }
            _ => None,
        })
    }

    fn landing_at(&self, l: &Hash) -> Result<Option<Location>, StoreError> {
        self.index.typed(kinds::LANDING, l)
    }
}

/// The live [`NoticeCapture`]s: each one's id, expected texts and the notices it took.
type NoticeCaptures = Vec<(u64, Vec<String>, Vec<String>)>;

static NOTICE_CAPTURES: Mutex<NoticeCaptures> = Mutex::new(Vec::new());
/// How many [`NoticeCapture`]s are live, so a store with none never takes the lock.
static NOTICE_CAPTURING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Writes one notice a library cannot return (the store's failed close in `Drop`, the MCP
/// server's failed republish after a panic) to stderr, unless a live [`NoticeCapture`] expects
/// it. A write whose error is dropped, not the print macro: that panics when stderr is closed,
/// and a panic in Drop during an unwind aborts the process.
pub fn notice(text: &str) {
    use std::io::Write as _;
    if NOTICE_CAPTURING.load(Ordering::SeqCst) > 0
        && let Ok(mut all) = NOTICE_CAPTURES.lock()
    {
        for (_, expected, taken) in all.iter_mut() {
            if expected.iter().any(|e| text.contains(e.as_str())) {
                taken.push(text.to_string());
                return;
            }
        }
    }
    let _ = writeln!(std::io::stderr(), "{text}");
}

/// Test hook: while the returned capture lives, a [`notice`] (any thread) that holds one of
/// `expected` (a fault test's store directory, say) goes to it instead of stderr, so the test
/// asserts the notice it provokes and every unexpected notice stays visible.
#[doc(hidden)]
pub fn capture_notices_for_tests(expected: &[&str]) -> NoticeCapture {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    NOTICE_CAPTURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, expected.iter().map(|e| e.to_string()).collect(), vec![]));
    NOTICE_CAPTURING.fetch_add(1, Ordering::SeqCst);
    NoticeCapture { id }
}

/// The store notices a test expects ([`capture_notices_for_tests`]); dropping it ends the
/// capture.
#[doc(hidden)]
pub struct NoticeCapture {
    id: u64,
}

impl NoticeCapture {
    /// The notices taken so far, in order.
    pub fn notices(&self) -> Vec<String> {
        NOTICE_CAPTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(id, _, _)| *id == self.id)
            .map(|(_, _, taken)| taken.clone())
            .unwrap_or_default()
    }
}

impl Drop for NoticeCapture {
    fn drop(&mut self) {
        NOTICE_CAPTURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(id, _, _)| *id != self.id);
        NOTICE_CAPTURING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Test hook: runs `f` (a drop, say) while the unclean-close notices of the stores at or under
/// `dir` go to the test instead of stderr, and returns them in order. Panics if one of them is
/// not an unclean-close notice of such a store. Every other notice stays visible.
#[doc(hidden)]
pub fn unclean_closes_for_tests(dir: &Path, f: impl FnOnce()) -> Vec<String> {
    let at = format!("rdy: the store at {}", dir.display());
    let capture = capture_notices_for_tests(&[&at]);
    f();
    let taken = capture.notices();
    for n in &taken {
        assert!(
            n.starts_with(&at)
                && n.contains(" did not close cleanly (")
                && n.ends_with("); the next open catches its index up from the log"),
            "{n}"
        );
    }
    taken
}

/// Test hook: drops `store`, whose close a fault test expects to fail (a poisoned store, say),
/// and returns the one notice that failure gives, which goes to the test instead of stderr
/// ([`unclean_closes_for_tests`]). Panics unless the drop gives exactly one.
#[doc(hidden)]
pub fn drop_unclean_for_tests(store: Store) -> String {
    let dir = store.dir.clone();
    let mut taken = unclean_closes_for_tests(&dir, || drop(store));
    assert_eq!(
        taken.len(),
        1,
        "one unclean-close notice of {} was expected: {taken:?}",
        dir.display()
    );
    taken.remove(0)
}

/// The notice of a store at `dir` whose close failed with `e`.
fn unclean_close_notice(dir: &Path, e: &StoreError) -> String {
    format!(
        "rdy: the store at {} did not close cleanly ({e}); the next open catches its index up from the log",
        dir.display()
    )
}

impl Drop for Store {
    /// Closes the store as `close` does. A failure leaves the sentinel, so the next open catches
    /// up; it cannot be returned from here, so it is a [`notice`] on stderr.
    fn drop(&mut self) {
        if let Err(e) = self.close_once() {
            notice(&unclean_close_notice(&self.dir, &e));
        }
    }
}

/// What the opened log's first sealed batch set: its mode and where it ends, read from the
/// origin to the first marker.
fn read_creation(log: &SegmentLog) -> Result<Creation, StoreError> {
    let mut first = FirstBatch::default();
    let walked = log.for_each_view(ORIGIN, |_, v, next| {
        match first.frame(v.kind, v.payload, next) {
            Ok(true) => Err(Walk::Done),
            Ok(false) => Ok(()),
            Err(e) => Err(Walk::Failed(e)),
        }
    });
    match walked {
        Ok(()) | Err(Walk::Done) => Ok(first.creation(log.end())),
        Err(Walk::Failed(e)) => Err(e),
    }
}

/// The features of the last sealed `meta/store_features` ref of the store at `dir`, read from
/// its segments with no lock and no writer; None when there is none or it is not a valid
/// manifest.
fn sealed_log_features(dir: &Path) -> Result<Option<StoreFeatures>, StoreError> {
    let value = crate::segment::sealed_ref_value(
        &dir.join("segments"),
        format::REF_STORE_FEATURES,
        format::MAX_MANIFEST_BYTES,
    )?;
    Ok(value.and_then(|v| StoreFeatures::parse(&v).ok()))
}

/// The store access of `features`, which permit reads only: the first feature by name that
/// this rdy does not register and whose class is read-only (a valid manifest is sorted by name).
fn read_only_access(features: &StoreFeatures) -> Access {
    let known = format::registered_features();
    let f = features
        .features
        .iter()
        .find(|f| {
            f.class == format::FeatureClass::ReadOnlyCompatible
                && !known.iter().any(|k| k.name == f.name)
        })
        .expect("a read-only manifest has an unknown read-only feature");
    Access::ReadOnly {
        feature: f.name.clone(),
        required_rdy: f.required_rdy.clone(),
    }
}

/// The upgrade plan of the closed store at `dir`: with this rdy's steps, or with `registry`'s
/// (a test hook's).
fn plan_upgrade(
    dir: &Path,
    registry: Option<&[crate::upgrade::UpgradeStep]>,
) -> Result<crate::upgrade::UpgradePlan, StoreError> {
    match registry {
        None => crate::upgrade::inspect(dir),
        Some(r) => crate::upgrade::inspect_with_registry_for_tests(dir, r),
    }
}

/// Applies `plan` to the store at `dir` under `lock`: with this rdy's steps, or with
/// `registry`'s (a test hook's).
fn apply_upgrade(
    dir: &Path,
    lock: &StoreLock,
    plan: &crate::upgrade::UpgradePlan,
    registry: Option<&[crate::upgrade::UpgradeStep]>,
) -> Result<crate::upgrade::UpgradeReport, StoreError> {
    match registry {
        None => crate::upgrade::apply_locked(dir, lock, plan),
        Some(r) => crate::upgrade::apply_locked_with_registry_for_tests(dir, lock, plan, r),
    }
}

/// The refusal of a holder's creation where a store, or anything else, already is.
fn already_exists(dir: &Path) -> StoreError {
    StoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "{} already holds something: a holder is created only in an empty directory",
            dir.display()
        ),
    ))
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
///
/// Every ref replayed meets the holder rules (`crate::holder::RefRules`) of the mode the log's
/// first batch set: `rules` are those of the log before `from` (`RefRules::of_log`), so a batch
/// that `from` cuts is judged whole at its marker.
fn replay_into(
    log: &SegmentLog,
    index: &Index,
    from: Location,
    fail_after: Option<u64>,
    mut rules: RefRules,
) -> Result<CatchUp, StoreError> {
    let end = log.end();
    let mut done = CatchUp::default();
    let mut entries = vec![];
    let mut in_chunk = 0usize;
    let auth = log.authorization();
    let held = ReplayHeld {
        index,
        log,
        auth: auth.as_ref(),
    };
    let mut r = Resolver::new(&held);
    log.for_each_view(from, |loc, v, next| {
        if fail_after == Some(done.frames) {
            return Err(StoreError::Db("injected replay failure".into()));
        }
        rules.on_frame(loc, v.kind, v.payload)?;
        // the redaction rows come from the validated stream only: an apply the bootstrap did not
        // validate is never a row, and a removed payload's entries come from its residue
        if (v.kind == kinds::REDACTION_APPLY && !auth.holds_apply(loc))
            || (v.kind == kinds::REDACTION_CARRIED && !auth.holds_carried(loc))
        {
            return Err(StoreError::corrupt_at(loc));
        }
        entries.extend(match v.payload {
            // a carried record's rows are the validated record's: no live writer writes one, so
            // only the bootstrap of an open or a reindex makes them
            Some(_) if v.kind == kinds::REDACTION_CARRIED => vec![
                IndexEntry::Object {
                    kind: v.kind,
                    hash: v.hash,
                    loc,
                },
                auth.carried_entry_at(loc)
                    .ok_or_else(|| StoreError::corrupt_at(loc))?,
            ],
            // a request's spans are the log's own: a holder's come from its approvals
            Some(payload) if v.kind == kinds::REDACTION => vec![
                IndexEntry::Object {
                    kind: v.kind,
                    hash: v.hash,
                    loc,
                },
                auth.request_entry(v.hash, payload)?,
            ],
            Some(payload) => entries_for_frame(v.kind, v.hash, loc, payload, &mut r)?,
            None => {
                r.note(crate::codec::logical_kind(v.kind), v.hash, loc)?;
                auth.entries_for_hole(v.kind, v.hash, loc)?
            }
        });
        done.frames += 1;
        in_chunk += 1;
        if in_chunk == CATCH_UP_CHUNK {
            index.apply(&entries, next)?;
            done.commits += 1;
            entries.clear();
            r.clear();
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
        format::write_format_file(dir, &format::baseline_features())?;
        synced.push(dir.to_path_buf());
        Store::open_current_format(dir, synced, None, None)
    }

    /// Creates a holder store (D-BPYN8) with repo id `repo_id`, in a directory that does not
    /// exist or is wholly empty; a directory holding anything is `StoreError::Io` of kind
    /// `AlreadyExists`, and nothing is written into it. The holder's first sealed batch holds its
    /// format, its repo id and its mode (`meta/store_mode` = `holder`), then a checkpoint makes
    /// the index durable. No origin is written. `FORMAT` is written last, so a crash before it
    /// leaves a directory that no open accepts, never a project: retry in a new directory. The
    /// store holds its lock as any open store does.
    pub fn create_holder(dir: &Path, repo_id: RepoId) -> Result<Store, StoreError> {
        let mut synced = match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_dir() && std::fs::read_dir(dir)?.next().is_none() => vec![],
            Ok(_) => return Err(already_exists(dir)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fsutil::create_dir_durable(dir)?,
            Err(e) => return Err(e.into()),
        };
        synced.push(dir.to_path_buf());
        let mut store = Store::open_current_format(dir, synced, None, Some(repo_id))?;
        format::write_format_file(dir, &store.features)?;
        store.synced.push(dir.to_path_buf());
        Ok(store)
    }

    /// Opens an existing store with [`OpenPolicy::AutoUpgrade`]. Opening never creates or
    /// claims a store (fmt3): a path that is not a directory with a `FORMAT` file whose features
    /// this rdy may read is refused before anything in it is created, locked, opened or written.
    /// A store with an unknown read-only feature opens for reading only (D-WWAK7).
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        Store::open_with_policy(dir, OpenPolicy::AutoUpgrade)
    }

    /// Opens an existing store as [`Store::open`] does, with upgrade policy `policy`.
    pub fn open_with_policy(dir: &Path, policy: OpenPolicy) -> Result<Store, StoreError> {
        Store::open_policy(dir, None, policy, None)
    }

    /// Opens an existing store as [`Store::open`] does, under `lock`, which the caller took on
    /// this store's lock file and keeps. The store shares the lock while it is open; the lock is
    /// released only when the caller drops it too. A lock that is not this store's is `Locked`:
    /// the caller does not hold the store.
    pub fn open_locked(dir: &Path, lock: &StoreLock) -> Result<Store, StoreError> {
        Store::open_locked_with_policy(dir, lock, OpenPolicy::AutoUpgrade)
    }

    /// [`Store::open_locked`] with upgrade policy `policy`.
    pub fn open_locked_with_policy(
        dir: &Path,
        lock: &StoreLock,
        policy: OpenPolicy,
    ) -> Result<Store, StoreError> {
        Store::check_access(dir)?;
        if !lock.guards(dir) {
            return Err(StoreError::Locked(dir.to_path_buf()));
        }
        Store::open_policy(dir, Some(lock), policy, None)
    }

    /// Test hook: [`Store::open_with_policy`] with the upgrade steps `registry` in place of this
    /// rdy's, through the same open. No CLI, MCP, config or environment path reaches it.
    #[doc(hidden)]
    pub fn open_with_registry_for_tests(
        dir: &Path,
        policy: OpenPolicy,
        registry: &[crate::upgrade::UpgradeStep],
    ) -> Result<Store, StoreError> {
        Store::open_policy(dir, None, policy, Some(registry))
    }

    /// The open every policy shares. `registry` is None for this rdy's upgrade steps; only the
    /// test hooks pass one.
    ///
    /// - A store this rdy may only read opens through [`Store::open_read_only`]: it is never
    ///   upgraded.
    /// - `RequireCurrent` refuses a store with planned steps or a pending completion before
    ///   anything is written.
    /// - `AutoUpgrade` with planned steps takes the store lock (or uses `held`), plans again
    ///   and applies the plan (`crate::upgrade::apply_locked`), then opens the upgraded store
    ///   under that lock. The open accepts a `FORMAT` the last upgrade receipt explains, and the
    ///   store's completion stays pending ([`Store::upgrade_pending`]) for the engine.
    fn open_policy(
        dir: &Path,
        held: Option<&StoreLock>,
        policy: OpenPolicy,
        registry: Option<&[crate::upgrade::UpgradeStep]>,
    ) -> Result<Store, StoreError> {
        if let Access::ReadOnly {
            feature,
            required_rdy,
        } = Store::check_access(dir)?
        {
            return Store::open_read_only(dir, held.cloned(), feature, required_rdy);
        }
        // a store the planner refuses keeps the open's own refusal: only a plan the planner
        // could make is applied, and the open below judges the store as it always did
        let (steps, pending) = match plan_upgrade(dir, registry) {
            Ok(plan) => (!plan.steps.is_empty(), plan.pending_check),
            Err(_) => (false, false),
        };
        match policy {
            OpenPolicy::RequireCurrent => {
                if steps || pending {
                    return Err(StoreError::UpgradeRequired);
                }
                Store::open_current_format_with(dir, vec![], held.cloned(), None, false)
            }
            OpenPolicy::AutoUpgrade => {
                let lock = if !steps {
                    held.cloned()
                } else {
                    let lock = match held {
                        Some(lock) => lock.clone(),
                        None => StoreLock::take(dir)?, // lock1
                    };
                    // under the lock, the plan read before it is read again: another open may
                    // have applied it meanwhile, which leaves only its completion
                    let plan = plan_upgrade(dir, registry)?;
                    apply_upgrade(dir, &lock, &plan, registry)?;
                    Some(lock)
                };
                Store::open_current_format_with(dir, vec![], lock, None, true)
            }
        }
    }

    /// The current locked open ([`Store::open_locked`]) without an upgrade policy: the upgrade
    /// transaction's own open of the store it upgrades. It also opens a store whose `FORMAT`
    /// still mirrors the manifest before a sealed upgrade batch, when the log's last upgrade
    /// receipt explains the difference (its `from` is `FORMAT`'s hash, its `to` the log's): the
    /// upgrade's completion runs its checks on that store, then publishes `FORMAT`
    /// (`crate::upgrade::publish_checked`). `FORMAT` is never put back or published here.
    pub(crate) fn open_unupgraded_locked(
        dir: &Path,
        lock: &StoreLock,
    ) -> Result<Store, StoreError> {
        Store::check_existing(dir)?;
        if !lock.guards(dir) {
            return Err(StoreError::Locked(dir.to_path_buf()));
        }
        Store::open_current_format_with(dir, vec![], Some(lock.clone()), None, true)
    }

    /// What this rdy may do with the store at `dir`, read before anything in it is created,
    /// locked, opened or written: [`Store::check_existing`], where a store this rdy may only
    /// read is `ReadOnly` with its first unknown read-only feature by name. The sealed log is
    /// the authority over a `FORMAT` that lists more than it (a first use whose batch never
    /// sealed). Reads only.
    fn check_access(dir: &Path) -> Result<Access, StoreError> {
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err(format::other_build(dir, "is not a directory")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format::no_store(dir));
            }
            Err(e) => return Err(e.into()),
        }
        let Some(mirror) = format::read_format_file(dir)? else {
            return Err(format::other_build(dir, "has no FORMAT file"));
        };
        let judged = match format::check_features(&mirror) {
            Ok(format::StoreAccess::ReadWrite) => return Ok(Access::ReadWrite),
            first => match sealed_log_features(dir)? {
                Some(log) if log.is_subset_of(&mirror) => log,
                _ => {
                    first?;
                    mirror
                }
            },
        };
        Ok(match format::check_features(&judged)? {
            format::StoreAccess::ReadWrite => Access::ReadWrite,
            format::StoreAccess::ReadOnly => read_only_access(&judged),
        })
    }

    /// The checks a writable open makes before it creates, locks, opens or writes anything:
    /// `dir` is a directory with a `FORMAT` file whose features this rdy may write. A `FORMAT`
    /// that lists more than its sealed log (a first use whose batch never sealed) is judged by
    /// the sealed log's features, read from the segments. A store this rdy may only read is
    /// refused with the feature's required build. Reads only.
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
            Some(f) => match format::check_writable(&f) {
                Ok(()) => Ok(()),
                // the sealed log is the authority: a FORMAT ahead of it (a first use whose batch
                // never sealed) does not refuse a log this rdy may write
                Err(refusal) => match sealed_log_features(dir)? {
                    Some(log) if log.is_subset_of(&f) => format::check_writable(&log),
                    _ => Err(refusal),
                },
            },
            None => Err(format::other_build(dir, "has no FORMAT file")),
        }
    }

    /// Opens a store whose `FORMAT` names the current store format (`STORE_FORMAT`). With
    /// `holder`, the log must be empty (`AlreadyExists` otherwise): its first batch makes the
    /// store a holder with that repo id.
    fn open_current_format(
        dir: &Path,
        synced: Vec<PathBuf>,
        held: Option<StoreLock>,
        holder: Option<RepoId>,
    ) -> Result<Store, StoreError> {
        Store::open_current_format_with(dir, synced, held, holder, false)
    }

    /// [`Store::open_current_format`]; with `upgrade_pending`, a `FORMAT` that the log's last
    /// upgrade receipt explains is accepted as it is ([`Store::open_unupgraded_locked`]).
    fn open_current_format_with(
        dir: &Path,
        mut synced: Vec<PathBuf>,
        held: Option<StoreLock>,
        holder: Option<RepoId>,
        upgrade_pending: bool,
    ) -> Result<Store, StoreError> {
        let segments = dir.join("segments");
        // dur1: each directory created here is fsynced into its parent before any write is acked.
        // Only `segments` itself is created, never `dir`: a store removed after the check above
        // (a failed load takes its store back) is not made again by an open that raced it.
        match std::fs::create_dir(&segments) {
            Ok(()) => {
                fsutil::sync_dir(dir)?;
                synced.push(dir.to_path_buf());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let fresh = !dir.join("index.redb").exists() || !dir.join("lock").exists();
        let lock = match held {
            Some(lock) => lock,
            None => StoreLock::take(dir)?, // lock1
        };
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
        // two passes: the redaction bootstrap validates every apply from the log's own frames,
        // then the marker rule's scan hashes every payload but the authorized holes
        let (log, quarantined) = SegmentLog::open_authorized(&segments, SEGMENT_MAX, acked)?;
        // the mode comes from the log's first batch, before any other ref is judged
        let creation = read_creation(&log)?;
        if holder.is_some() && log.end() != ORIGIN {
            return Err(already_exists(dir));
        }
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
                refs_read: AtomicU64::new(0),
                carried_frames_read: AtomicU64::new(0),
                epoch: Arc::new(AtomicU64::new(0)),
                poisoned: AtomicBool::new(false),
                pending: AtomicBool::new(false),
                access: Access::ReadWrite,
                _hold: None,
            }),
            tracking: Arc::default(),
            epoch: 0,
            preview: None,
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
            held: false,
            open: None,
            stale_index: false,
            creation,
            features: format::baseline_features(),
            held_features: None,
            accept_pending: upgrade_pending,
            completion_pending: false,
            synced_unindexed: false,
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
        // the log's features are the store's: one this rdy may not write, or one FORMAT does not
        // list, is refused before the index moves
        if let Some(f) = store.features_ref()? {
            store.check_log_features(&f, false)?;
        }
        // the index's redaction rows must be the ones the log's validated records give: a
        // forged or forgotten row is rebuilt from the log, never trusted
        if store.ensure_redaction_index(upto)? {
            store.report.index_rebuilt = true;
        }
        let caught_up = store.catch_up()?;
        store.report.caught_up_frames = caught_up.frames;
        store.report.catch_up_commits = caught_up.commits;
        store.ensure_format(holder)?;
        // the index's mode and a holder's refs are the ones the log's first batch allows
        store.check_mode_against(store.creation)?;
        // an authorized removal finishes before any reader escapes this open
        store.finish_redactions_at_open()?;
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

    /// Opens the store at `dir`, whose features permit reads only (D-WWAK7), for reading only:
    /// no byte of the source store changes. It takes the existing lock file's exclusive lock
    /// without creating it (a missing one is `ReadOnlyNoLock`), or uses `held`, and every
    /// reader holds the lock until the last one drops, so no process writes or removes content
    /// beneath a read.
    ///
    /// The sealed log is read by the writable open's rules with no writer
    /// ([`SegmentLog::open_read_only`]): no sentinel, no quarantine, no cut, no `FORMAT`
    /// repair, no source index, no removal finished. An unacknowledged tail is ignored and
    /// reported as an unclean shutdown; damage an acknowledged batch follows is refused. The
    /// index is a private one in a temporary directory, built from the log by the replay every
    /// open uses, and removed when the last reader drops (or at once, when the replay fails).
    /// An authorized removal that has not finished is `RedactionPending` before the store is
    /// returned.
    fn open_read_only(
        dir: &Path,
        held: Option<StoreLock>,
        feature: String,
        required_rdy: String,
    ) -> Result<Store, StoreError> {
        let segments = dir.join("segments");
        let lock = match held {
            Some(lock) => lock,
            None => StoreLock::take_existing(dir)?,
        };
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
        let unclean = std::fs::symlink_metadata(dir.join(INDEX_OPEN_SENTINEL)).is_ok();
        let (log, tail) = SegmentLog::open_read_only(&segments, SEGMENT_MAX, dir)?;
        let creation = read_creation(&log)?;
        // the private index: built here and owned by every reader; a failed build removes it
        let private = tempfile::Builder::new()
            .prefix(".rdy-read-only-")
            .tempdir()?;
        let index = Index::open(&private.path().join("index.redb"))?;
        replay_into(&log, &index, ORIGIN, None, RefRules::new(creation))?;
        let reader = StoreReader {
            index: Arc::new(index),
            shared: Arc::new(ReaderShared {
                dir: dir.to_path_buf(),
                segments: SegmentReader::open(&segments),
                sets: Mutex::new(HashMap::new()),
                frames_read: AtomicU64::new(0),
                refs_read: AtomicU64::new(0),
                carried_frames_read: AtomicU64::new(0),
                epoch: Arc::new(AtomicU64::new(0)),
                poisoned: AtomicBool::new(false),
                pending: AtomicBool::new(false),
                access: Access::ReadOnly {
                    feature,
                    required_rdy,
                },
                _hold: Some(ReadOnlyHold {
                    _lock: lock.clone(),
                    dir: private,
                }),
            }),
            tracking: Arc::default(),
            epoch: 0,
            preview: None,
        };
        let end = log.end();
        let mut store = Store {
            dir: dir.to_path_buf(),
            log,
            reader: Some(reader),
            _lock: lock,
            faults: Faults::default(),
            poisoned: false,
            synced: vec![],
            report: OpenReport {
                // an ignored tail is the leftover of a writer that died in a batch
                unclean_shutdown: unclean || tail,
                ..OpenReport::default()
            },
            opened: false,
            pending: 0,
            last_write: Instant::now(),
            checkpoint_frames: CHECKPOINT_FRAMES,
            checkpoints: 0,
            checkpoint_error: None,
            writes: 0,
            last_batch: vec![],
            last_batch_end: ORIGIN,
            held: false,
            open: None,
            stale_index: false,
            creation,
            features: format::baseline_features(),
            held_features: None,
            accept_pending: false,
            completion_pending: false,
            synced_unindexed: false,
        };
        // the log's features are the store's; FORMAT is judged as the writable open judges it,
        // and never repaired
        let Some(f) = store.features_ref()? else {
            return Err(format::other_build(dir, "has a log with no store features"));
        };
        format::check_features(&f)?;
        match format::read_format_file(dir)? {
            Some(mirror) if mirror == f || f.is_subset_of(&mirror) => {}
            Some(mirror) if store.receipt_explains(&mirror, &f)? => {}
            _ => return Err(format::unmirrored(dir)),
        }
        store.features = f;
        store.check_mode_against(store.creation)?;
        store.refuse_unfinished_removal()?;
        debug_assert_eq!(store.index().indexed_upto()?, end);
        store.opened = true;
        Ok(store)
    }

    /// `RedactionPending` when an authorized removal of this read-only store's log has not
    /// finished (a payload it authorizes is not all zero yet): no read handle escapes before a
    /// writable open finishes it. A span only a carried record explains that is not all zero is
    /// `RedactionCorrupt`, as the writable open finds it.
    fn refuse_unfinished_removal(&self) -> Result<(), StoreError> {
        let auth = Arc::clone(self.log.authorization());
        for (loc, hole) in &auth.holes {
            if self.log.hole_state(*loc, hole)? == Some(true) {
                continue;
            }
            if !auth.erasable(loc) {
                return Err(StoreError::corrupt_at(*loc));
            }
            return Err(StoreError::RedactionPending);
        }
        Ok(())
    }

    /// What this store's open permits: `ReadOnly` for a store with an unknown read-only
    /// feature (D-WWAK7). Its readers keep it ([`StoreReader::access`]).
    pub fn access(&self) -> format::StoreAccess {
        StoreReader::access(self)
    }

    /// The one gate of every write (append, ref, seal, take, removal, reindex): a read-only
    /// store refuses with `ReadOnly` (E_STORE_READ_ONLY), a store whose upgrade completion is
    /// pending with `UpgradeRequired`. It runs before a feature is activated, a held batch
    /// changes or a file is created.
    pub fn ensure_writable(&self) -> Result<(), StoreError> {
        StoreReader::ensure_writable(self)?;
        if self.completion_pending {
            return Err(StoreError::UpgradeRequired);
        }
        Ok(())
    }

    /// Whether a sealed upgrade of this store waits for its completion: the engine's full
    /// fsck, then `crate::upgrade::publish_checked`. Writes are refused until then.
    pub fn upgrade_pending(&self) -> bool {
        self.completion_pending
    }

    /// Reads `FORMAT` again after the upgrade's completion published it: the completion is no
    /// longer pending once the mirror lists the log's features. Returns whether it still is.
    pub fn refresh_upgrade_pending(&mut self) -> Result<bool, StoreError> {
        if self.completion_pending {
            let mirror = format::read_format_file(&self.dir)?;
            self.completion_pending = !mirror.is_some_and(|m| self.features.is_subset_of(&m));
        }
        Ok(self.completion_pending)
    }

    /// The log's `meta/store_features` ref, as the index holds it; an invalid manifest is the
    /// manifest refusal.
    fn features_ref(&self) -> Result<Option<StoreFeatures>, StoreError> {
        self.index()
            .get_ref(format::REF_STORE_FEATURES)?
            .map(|v| format::decode_features(&v))
            .transpose()
    }

    /// The log's historical `meta/store_format` ref, as the index holds it.
    fn scalar_ref(&self) -> Result<Option<u32>, StoreError> {
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

    /// Refuses log features `f` that this rdy may not write, and features `FORMAT` does not
    /// list (`FORMAT` may list more: a first use whose batch did not seal). Then they are the
    /// store's. With `repair` (`f` is the caught-up log's), a `FORMAT` that lists more is put
    /// back to `f`: the sealed log is the authority.
    fn check_log_features(&mut self, f: &StoreFeatures, repair: bool) -> Result<(), StoreError> {
        format::check_writable(f)?;
        match format::read_format_file(&self.dir)? {
            Some(mirror) if mirror == *f => {}
            Some(mirror) if f.is_subset_of(&mirror) => {
                if repair {
                    format::write_format_file(&self.dir, f)?;
                }
            }
            Some(mirror) if self.receipt_explains(&mirror, f)? => {
                // a sealed upgrade the engine has not completed: only an open that completes
                // it (or the upgrade's own) accepts it, and it refuses writes until then
                if !self.accept_pending {
                    return Err(StoreError::UpgradeRequired);
                }
                self.completion_pending = true;
            }
            _ => return Err(format::unmirrored(&self.dir)),
        }
        self.features = f.clone();
        Ok(())
    }

    /// Whether the log's last upgrade receipt (`meta/upgrade`, as the index holds it) took the
    /// store from `mirror` to `log`: a `FORMAT` the upgrade has not published yet.
    fn receipt_explains(
        &self,
        mirror: &StoreFeatures,
        log: &StoreFeatures,
    ) -> Result<bool, StoreError> {
        let Some(v) = self.index().get_ref(format::REF_UPGRADE)? else {
            return Ok(false);
        };
        Ok(format::UpgradeReceipt::parse(&v)
            .is_ok_and(|r| r.from == mirror.hash() && r.to == log.hash()))
    }

    /// Makes the log hold the store's features, which `FORMAT` (already checked) mirrors. The
    /// log's ref is the truth. A log with frames and no features ref is refused: an index with
    /// no ref over a log with frames may have lost it (an old partial rebuild), so the index is
    /// first rebuilt from the log and asked again; a log that holds only a scalar store format is
    /// refused by that number. A new store (an empty log; `create` wrote `FORMAT` first) gets the
    /// ref and its repo id (`meta/repo_id`) as one marked batch, then a checkpoint. A crash
    /// before that leaves `FORMAT` and an empty log, which the next open finishes.
    /// (`create_holder` writes `FORMAT` only after this.)
    ///
    /// With `holder`, the new store's batch is a holder's: the features, the repo id `holder`
    /// names and `meta/store_mode`.
    fn ensure_format(&mut self, holder: Option<RepoId>) -> Result<(), StoreError> {
        let held_frames = self.log.end()
            != Location {
                segment: 0,
                offset: 0,
            };
        match self.features_ref()? {
            Some(f) => self.check_log_features(&f, true)?,
            None if held_frames => {
                // an index that lost the ref (an old partial rebuild) is rebuilt from the log;
                // a log that really has no features ref is still refused
                self.reindex()?;
                self.report.index_rebuilt = true;
                match self.features_ref()? {
                    Some(f) => self.check_log_features(&f, true)?,
                    None => {
                        return Err(match self.scalar_ref()? {
                            Some(n) => format::scalar_refusal(&self.dir, n),
                            None => {
                                format::other_build(&self.dir, "has a log with no store features")
                            }
                        });
                    }
                }
            }
            None => {
                // the features FORMAT mirrors (a new store's baseline), or the baseline for a
                // holder, whose FORMAT comes after this batch
                let features = match holder {
                    Some(_) => format::baseline_features(),
                    None => format::read_format_file(&self.dir)?
                        .unwrap_or_else(format::baseline_features),
                };
                // one batch of the features and the repo id (D-DAGNB): set_refs refuses meta/
                // refs
                let mut refs = vec![
                    (format::REF_STORE_FEATURES.into(), features.to_json()),
                    (
                        crate::meta::REF_REPO_ID.into(),
                        crate::meta::encode_repo_id(&holder.unwrap_or_else(RepoId::new)),
                    ),
                ];
                // a holder's mode is the creation's (D-BPYN8): this batch is the only place
                if holder.is_some() {
                    refs.push((
                        crate::holder::REF_STORE_MODE.into(),
                        crate::holder::HOLDER_MODE.to_vec(),
                    ));
                }
                let items = self.ref_items(refs);
                self.write_refs(items, RefWrite::Unchecked)?;
                self.creation = Creation {
                    mode: if holder.is_some() {
                        StoreMode::Holder
                    } else {
                        StoreMode::Project
                    },
                    end: self.log.end(),
                };
                self.features = features;
                // a new store is durable from its first open
                self.checkpoint()?;
            }
        }
        Ok(())
    }

    /// The features the log's latest `meta/store_features` ref holds.
    pub fn features(&self) -> &StoreFeatures {
        &self.features
    }

    /// The upgrade transaction's commit (D-WWAK7): exactly the features ref (`features`) and the
    /// receipt ref (`receipt`, an [`format::UpgradeReceipt`]'s JSON) in one unheld batch. Refused
    /// with a held batch open, a receipt that does not validate, a receipt whose `source_end` is
    /// not the current sealed end, whose `from` is not the current manifest's hash, or whose `to`
    /// is not `features`' hash. Returns the batch's `BATCH_COMMIT` hash. No public ref write
    /// passes these checks.
    ///
    /// The marker's sync is the commit point. A failure before it rolls the batch back, as a
    /// failed write does. A failure after it (the index update) keeps the batch: the store's
    /// features become the batch's, the index is rebuilt from the log, and the error is
    /// returned, so the receipt waits for the upgrade's completion.
    pub(crate) fn commit_upgrade_refs(
        &mut self,
        features: &StoreFeatures,
        receipt: &[u8],
    ) -> Result<Hash, StoreError> {
        if self.open.is_some() {
            return Err(batch_open("before the upgrade batch"));
        }
        let r = format::UpgradeReceipt::parse(receipt).map_err(StoreError::Refused)?;
        if r.source_end != self.log.end() {
            return Err(StoreError::Refused(
                "the upgrade receipt does not start at the sealed end".into(),
            ));
        }
        if r.from != self.features.hash() {
            return Err(StoreError::Refused(
                "the upgrade receipt does not start from the store's features".into(),
            ));
        }
        if r.to != features.hash() {
            return Err(StoreError::Refused(
                "the upgrade receipt does not end at the new features".into(),
            ));
        }
        features.validate().map_err(|e| e.store_error())?;
        let items = self.ref_items(vec![
            (format::REF_STORE_FEATURES.into(), features.to_json()),
            (format::REF_UPGRADE.into(), receipt.to_vec()),
        ]);
        // where the batch starts: the end, or the next segment's start when it rolls
        let start = self.log.batch_start(&items);
        let held = self.held;
        self.held = false;
        self.synced_unindexed = false;
        let wrote = self.write_refs(items, RefWrite::Upgrade);
        self.held = held;
        if let Err(e) = wrote {
            if std::mem::take(&mut self.synced_unindexed) {
                // past the commit point: the batch is durable, and the features are the log's.
                // The index is rebuilt from the log; if that fails too, the handle is poisoned
                // and its close removes the index, which the next open rebuilds
                self.features = features.clone();
                if self.reindex().is_err() {
                    self.poison();
                }
            }
            return Err(e);
        }
        self.features = features.clone();
        // the batch's marker, as the log wrote it: the last frame from the batch's start
        self.log
            .scan_from(start)?
            .last()
            .filter(|(_, kind, _)| *kind == kinds::BATCH_COMMIT)
            .map(|(_, _, hash)| *hash)
            .ok_or_else(|| StoreError::Decode("the upgrade batch has no marker".into()))
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

    /// A reader of this store: a clone of its own (rdr1), which captures the family's redaction
    /// epoch now.
    pub fn reader(&self) -> StoreReader {
        let mut r = StoreReader::clone(self);
        r.epoch = r.shared.epoch.load(Ordering::Acquire);
        r.preview = None;
        r
    }

    /// A reader of this store that reads through `preview`'s masks too.
    pub(crate) fn reader_with_preview(
        &self,
        preview: Arc<crate::redaction::Preview>,
    ) -> StoreReader {
        let mut r = self.reader();
        r.preview = Some(preview);
        r
    }

    /// Poisons the store: every later write fails until reopen, and its readers' epoch checks
    /// fail.
    fn poison(&mut self) {
        self.poisoned = true;
        if let Some(r) = &self.reader {
            r.shared.poisoned.store(true, Ordering::Release);
        }
    }

    /// The logical kind and hash of every data frame the last successful `write` appended, after
    /// dedupe and in write order (batch markers excluded); empty right after open and after a write
    /// that appended nothing. A failed write leaves it as it was. With held batches, the last
    /// sealed batch: a held write sets it only when it appended nothing and no batch is open.
    pub fn last_batch(&self) -> &[(u8, Hash)] {
        &self.last_batch
    }

    /// The end of the log right after the last successful `write` (with held batches, the last
    /// seal); the origin right after open.
    pub fn last_batch_end(&self) -> Location {
        self.last_batch_end
    }

    /// Turns held batches on or off (plan M2e2, D-Z0EMP). Turning them off seals an open batch
    /// first.
    pub fn set_held(&mut self, on: bool) -> Result<(), StoreError> {
        if !on && self.open.is_some() {
            self.seal(vec![])?;
        }
        self.held = on;
        Ok(())
    }

    /// Whether held batches are on.
    pub(crate) fn held(&self) -> bool {
        self.held
    }

    /// The kind and hash of each frame of the open held batch, in order; `None` when no batch is
    /// open.
    /// (Each frame by its logical kind, `crate::codec`.)
    pub fn open_batch(&self) -> Option<&[(u8, Hash)]> {
        self.open.as_ref().map(|o| &o.frames[..])
    }

    /// Ends the held batch: appends `last` (the batch's TRANSITION frame, or nothing) to it,
    /// writes the marker over every frame of the batch and syncs once, then indexes `last`. One
    /// sync per batch, held or not. With no batch open and `last` empty it does nothing; with no
    /// batch open and `last` not empty it opens one first (a reload batch holds only its record).
    ///
    /// Like `write`, it refuses a redaction record (kinds 20 to 23) and an item that would bring
    /// back a removed hash, before anything is appended. If the batch holds a redaction apply,
    /// `last` must be empty or one TRANSITION frame (`Refused` otherwise, the batch still open).
    ///
    /// A failure truncates the log to the batch's start. If a batch was open, it also poisons the
    /// store: the index already holds entries of frames that are gone, so every later write and
    /// every checkpoint fails until reopen. If no batch was open, the index holds nothing of the
    /// batch, so the store keeps working, as after a failed write. The error is returned.
    pub fn seal(&mut self, last: Vec<(u8, Vec<u8>)>) -> Result<(), StoreError> {
        let was_open = self.open.is_some();
        if !was_open && last.is_empty() {
            return Ok(());
        }
        self.ensure_writable()?;
        if self.poisoned {
            return Err(self.poisoned_err());
        }
        // the same refusals as `write`, before anything is appended: a redaction record (only
        // the store's checked redaction writers write one) or an item that would bring back a
        // removed hash is refused with the batch still open, and nothing poisoned
        crate::redaction::refuse_redaction_kinds(&last)?;
        crate::plan::refuse_plan_writes(&last)?;
        self.check_refs(&last, RefWrite::Public)?;
        self.refuse_removed(&last)?;
        self.check_new_signatures(&last)?;
        // a held batch that holds a redaction apply seals through the apply hook, which never
        // takes the rollback below once the batch is synced
        if self.open.as_ref().is_some_and(|o| {
            o.frames
                .iter()
                .any(|(k, _)| crate::codec::logical_kind(*k) == kinds::REDACTION_APPLY)
        }) {
            // the batch's features become the store's at its sync (`seal_held_apply`); a refusal
            // keeps the batch open with them, and only a confirmed rollback puts FORMAT back
            return self.seal_held_apply(last).map(|_| ());
        }
        // `last` is decoded before anything is appended: a value that does not decode is
        // refused with the batch still open, and nothing poisoned
        let mut probe = Resolver::new(&NoneHeld);
        for (kind, payload) in &last {
            entries_for_frame(*kind, Hash::of(payload), ORIGIN, payload, &mut probe)?;
        }
        let (last, activated, published) = self.first_use(last)?;
        let start = self.open.as_ref().map_or(self.log.end(), |o| o.start);
        match self.seal_batch(last) {
            Ok(()) => {
                if let Some(f) = activated.or_else(|| self.held_features.take()) {
                    self.features = f;
                }
                self.held_features = None;
                Ok(())
            }
            Err(e) => {
                let ahead = published || self.held_features.is_some();
                self.held_features = None;
                self.open = None;
                if was_open {
                    self.poison();
                    self.stale_index = true;
                }
                // as in `write`: a log that cannot be rolled back poisons the store, and keeps
                // the FORMAT it published (its marked batch may still be in the log; the next
                // open judges by the log)
                if self.roll_back(start) && ahead {
                    self.restore_mirror();
                }
                Err(e)
            }
        }
    }

    /// `seal` once `last` decoded: its frames are indexed at their places once appended.
    fn seal_batch(&mut self, last: Vec<(u8, Vec<u8>)>) -> Result<(), StoreError> {
        let placed = self.log.append_frames(&last, self.open.is_none())?;
        if let Some((loc, _)) = placed.first() {
            self.open.get_or_insert(OpenBatch {
                start: *loc,
                frames: vec![],
            });
        }
        let open = self.open.as_mut().expect("a batch is open");
        open.frames.extend(
            last.iter()
                .zip(&placed)
                .map(|((k, _), (_, h))| (crate::codec::logical_kind(*k), *h)),
        );
        let placed_entries = self.placed_entries(&last, &placed)?;
        self.log.crash_point(CrashPoint::BeforeMarker);
        let open = self.open.as_ref().expect("a batch is open");
        self.log
            .append_marker(open.start, open.frames.iter().map(|(_, h)| h))?;
        self.log.crash_point(CrashPoint::BeforeSync);
        self.log.sync()?;
        self.log.crash_point(CrashPoint::AfterSync);
        if self.take_index_fault() {
            return Err(StoreError::Db("injected index failure".into()));
        }
        self.index().apply(&placed_entries, self.log.end())?;
        self.log.crash_point(CrashPoint::AfterIndex);
        let open = self.open.take().expect("a batch is open");
        self.acknowledge(open.frames);
        Ok(())
    }

    /// The index entries of `frames`, appended at `placed`, in order: each read against the
    /// store and the frames before it.
    fn placed_entries(
        &self,
        frames: &[(u8, Vec<u8>)],
        placed: &[(Location, Hash)],
    ) -> Result<Vec<IndexEntry>, StoreError> {
        let reader = self.reader();
        let mut r = Resolver::new(&reader);
        let mut out = vec![];
        for ((kind, payload), (at, hash)) in frames.iter().zip(placed) {
            out.extend(entries_for_frame(*kind, *hash, *at, payload, &mut r)?);
        }
        Ok(out)
    }

    /// What a batch does once it is durable in the log: it becomes `last_batch`, and the index
    /// may checkpoint.
    fn acknowledge(&mut self, frames: Vec<(u8, Hash)>) {
        // The write is acknowledged: it is durable in the log. A checkpoint is only the index
        // catching up, so a failure of one is recorded and retried, never returned.
        self.pending += frames.len() as u64 + 1;
        // the batch's objects by their logical kinds (`crate::codec`)
        self.last_batch = frames
            .into_iter()
            .map(|(k, h)| (crate::codec::logical_kind(k), h))
            .collect();
        self.last_batch_end = self.log.end();
        self.last_write = Instant::now();
        if self.pending >= self.checkpoint_frames
            && let Err(e) = self.checkpoint()
        {
            self.checkpoint_error = Some(e.to_string());
        }
    }

    /// Whether the injected index fault fires for this batch (`fail_next_index_apply`,
    /// `fail_index_apply_after`).
    fn take_index_fault(&mut self) -> bool {
        let after = match self.faults.index_apply_after {
            Some(0) => self.faults.index_apply_after.take().is_some(),
            Some(n) => {
                self.faults.index_apply_after = Some(n - 1);
                false
            }
            None => false,
        };
        std::mem::take(&mut self.faults.index_apply) || after
    }

    /// Drops the store's own reader, and with it the store's `Arc` of the index, keeping what
    /// readers share. The database closes now unless a cloned reader still holds it.
    fn drop_index(&mut self) -> Option<(Arc<ReaderShared>, Arc<Tracking>)> {
        self.reader.take().map(
            |StoreReader {
                 index,
                 shared,
                 tracking,
                 ..
             }| {
                drop(index);
                (shared, tracking)
            },
        )
    }

    /// Makes `index` the store's index, in a new reader that shares everything else with the old.
    fn set_index(&mut self, (shared, tracking): (Arc<ReaderShared>, Arc<Tracking>), index: Index) {
        let epoch = shared.epoch.load(Ordering::Acquire);
        self.reader = Some(StoreReader {
            index: Arc::new(index),
            shared,
            tracking,
            epoch,
            preview: None,
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
    ///
    /// The ref rules (`crate::holder`) judge the log, not the index: a holder's catch-up first
    /// walks the log up to `indexed_upto`, even with nothing left to replay, then replays with
    /// those rules. A project's rules hold no state across frames (a mode ref is refused outside
    /// the first batch, which sets none), and any mode ref the index holds fails the open's
    /// `check_mode_against`, so a project's catch-up skips that walk.
    fn catch_up(&mut self) -> Result<CatchUp, StoreError> {
        let from = self.index().indexed_upto()?;
        let end = self.log.end();
        if from < end && self.index_point_unusable(from, end)? {
            // close the file before it is renamed
            let shared = self.drop_index().expect("the store is open until close");
            let path = self.dir.join("index.redb");
            set_aside(&path)?;
            self.set_index(shared, Index::open(&path)?);
            self.report.index_rebuilt = true;
            return self.replay_from(ORIGIN, RefRules::new(self.creation));
        }
        let rules = match self.creation.mode {
            StoreMode::Project => RefRules::new(self.creation),
            StoreMode::Holder => RefRules::of_log(
                self.segments(),
                &self.log.authorization().holes,
                self.creation,
                from.min(end),
            )?,
        };
        if from >= end {
            return Ok(CatchUp::default());
        }
        self.replay_from(from, rules)
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
        Ok(from.offset < len && !self.log.frame_readable(from)?)
    }

    fn replay_from(&mut self, from: Location, rules: RefRules) -> Result<CatchUp, StoreError> {
        let fail_after = self.faults.replay_after.take();
        let done = replay_into(&self.log, self.index(), from, fail_after, rules)?;
        // the replayed frames are in the log, not yet in the durable index
        self.checkpoint()?;
        Ok(done)
    }

    /// Reads and hash-checks every frame in every segment and checks every batch marker;
    /// returns the number of data frames (markers are not counted).
    pub fn verify(&self) -> Result<usize, StoreError> {
        if self.open.is_some() {
            return Err(batch_open("first"));
        }
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
        self.ensure_writable()?;
        if self.open.is_some() {
            return Err(batch_open("first"));
        }
        let path = self.dir.join("index.redb");
        let tmp = self.dir.join(INDEX_TMP);
        // a leftover temp is an older failed rebuild; `Index::open` would reuse it
        remove_if_present(&tmp)?;
        let fail_after = self.faults.replay_after.take();
        // an error at any step, the open of the temp included, removes the temp
        let built = Index::open(&tmp).and_then(|ix| {
            replay_into(
                &self.log,
                &ix,
                ORIGIN,
                fail_after,
                RefRules::new(self.creation),
            )?;
            // durable before it becomes the index
            ix.checkpoint()?;
            crate::redaction_io::hit(crate::redaction_io::Failpoint::BeforeIndexPublish)?;
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
        self.stale_index = false;
        fsutil::sync_dir(&self.dir)?;
        Ok(())
    }

    /// Writes `items` as one batch: one marker and one sync. With held batches on, the frames
    /// join the open batch instead (no marker, no sync; `seal` ends it), and are indexed at once;
    /// a held batch that holds a redaction apply takes no more writes until its seal.
    ///
    /// A redaction record (kinds 20 to 23) is refused: only the store's checked redaction writers
    /// write one. So is an item whose hash a redaction removed (`Redacted`, naming the request):
    /// a removed block, change or chunk never comes back, nor does a blob that shares a removed
    /// chunk, nor a change that embeds a removed block version. Nothing is written then.
    ///
    /// The mode ref is never written (`store mode cannot change`); in a holder, a ref outside its
    /// families, or an `answer/tag-object:` ref, is refused (`crate::holder`).
    pub fn write(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        crate::plan::refuse_plan_writes(&items)?;
        self.write_refs(items, RefWrite::Public)
    }

    /// `write` with the ref rules `how` names.
    fn write_refs(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
        how: RefWrite,
    ) -> Result<Vec<Hash>, StoreError> {
        crate::redaction::refuse_redaction_kinds(&items)?;
        self.write_checked(items, how)
    }

    /// The take's write (Task 11 is its caller): `write`, where a holder's
    /// `answer/tag-object:<hash>` names a TAG object in `items` or in the store (O11).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn write_take(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
    ) -> Result<Vec<Hash>, StoreError> {
        self.write_refs(items, RefWrite::Take)
    }

    /// The ref rules of a write or seal of `items`, before anything is appended.
    fn check_refs(&self, items: &[(u8, Vec<u8>)], how: RefWrite) -> Result<(), StoreError> {
        self.check_mode_refs(items, how)?;
        crate::holder::refuse_upgrade_writes(items, how)
    }

    /// The mode and holder ref rules of [`Store::check_refs`] alone.
    fn check_mode_refs(&self, items: &[(u8, Vec<u8>)], how: RefWrite) -> Result<(), StoreError> {
        crate::holder::check_ref_writes(self.creation.mode, items, how, |h| {
            self.has_kind(kinds::TAG, h)
        })
    }

    /// `write` for the store's own checked writers: any kind, with the removed-hash refusal.
    pub(crate) fn write_guarded(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
    ) -> Result<Vec<Hash>, StoreError> {
        self.write_checked(items, RefWrite::Public)
    }

    /// `write_guarded` with the ref rules `how` names.
    fn write_checked(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
        how: RefWrite,
    ) -> Result<Vec<Hash>, StoreError> {
        self.ensure_writable()?;
        if self.poisoned {
            return Err(self.poisoned_err());
        }
        self.check_refs(&items, how)?;
        // a held batch that holds a redaction apply takes only its seal: its TRANSITION is the
        // last data frame, and no copy of the content it removes can join it before the seal
        if self.held
            && self.open.as_ref().is_some_and(|o| {
                o.frames
                    .iter()
                    .any(|(k, _)| crate::codec::logical_kind(*k) == kinds::REDACTION_APPLY)
            })
        {
            return Err(StoreError::Refused(
                "the held batch holds a redaction apply: seal it before writing more".into(),
            ));
        }
        self.refuse_removed(&items)?;
        self.check_new_signatures(&items)?;
        let (items, activated, published) = self.first_use(items)?;
        let start = self.log.end();
        let mut result = if self.held {
            self.append_held(items)
        } else {
            self.append_and_index(items)
        };
        // the caller's hashes are its own items': the features ref a first use added is not one
        if let (Ok(hashes), Some(_)) = (&mut result, &activated) {
            hashes.pop();
        }
        if result.is_ok() {
            self.writes += 1;
            if let Some(f) = activated {
                if self.held && self.open.is_some() {
                    self.held_features = Some(f);
                } else {
                    self.features = f;
                }
            }
        }
        if result.is_err() && how == RefWrite::Upgrade && self.synced_unindexed {
            // the upgrade's commit point is its marker's sync (D-WWAK7): its batch stays in the
            // log, and the index, which lacks it, is stale until `commit_upgrade_refs` rebuilds it
            self.stale_index = true;
            return result;
        }
        self.synced_unindexed = false;
        if result.is_err() {
            // nothing past `start` was acknowledged: discard it — including bytes a partial
            // append left past the end and a segment a failed roll left behind — so neither a
            // later write, a verify nor a reindex can see it. The frames of earlier writes of an
            // open held batch are before `start`, and stay open.
            // Only a confirmed rollback puts FORMAT back: a batch that stays in the log, marked
            // (its index update failed after the sync), uses what FORMAT lists now.
            if self.roll_back(start) && published {
                self.restore_mirror();
            }
        }
        result
    }

    /// The first use of a feature (D-WWAK7): when the new objects of `items` use features the
    /// store's manifest (or the open held batch's) does not name, `FORMAT` is made to list the
    /// merged manifest, then the merged manifest's `meta/store_features` ref joins `items`, so the
    /// batch that first uses a feature activates it. A batch that fails and rolls back appends
    /// neither, and its caller puts `FORMAT` back ([`Store::restore_mirror`]); one whose rollback
    /// fails keeps `FORMAT` as published. A `FORMAT` that lists more than the sealed log (a crash
    /// before the batch sealed) opens by the log's features, and the open puts it back. Returns the items, the merged manifest, if any, and whether `FORMAT` was
    /// rewritten.
    fn first_use(&self, mut items: Frames) -> Result<FirstUse, StoreError> {
        let current = self.held_features.as_ref().unwrap_or(&self.features);
        let mut needed: Vec<format::StoreFeature> = vec![];
        for (raw, payload) in &items {
            let kind = crate::codec::logical_kind(*raw);
            if kind == kinds::REF_UPDATE || kind == kinds::BATCH_COMMIT {
                continue;
            }
            for f in format::required_features(*raw, payload) {
                if !current.has(&f.name) && !needed.contains(&f) {
                    needed.push(f);
                }
            }
        }
        if needed.is_empty() {
            return Ok((items, None, false));
        }
        let merged = current.merged(&needed);
        // the merged manifest keeps the manifest's bounds, or nothing changes
        merged.validate_written().map_err(|e| e.store_error())?;
        let published = self.publish_mirror(&merged)?;
        items.extend(self.ref_items(vec![(format::REF_STORE_FEATURES.into(), merged.to_json())]));
        Ok((items, Some(merged), published))
    }

    /// Makes `FORMAT` list `features` before the batch that activates them seals, so no rdy
    /// that does not know them opens the store meanwhile. Returns whether it rewrote `FORMAT`.
    fn publish_mirror(&self, features: &StoreFeatures) -> Result<bool, StoreError> {
        let mirror = format::read_format_file(&self.dir)?;
        if mirror.is_some_and(|m| features.is_subset_of(&m)) {
            return Ok(false);
        }
        format::write_format_file(&self.dir, features)?;
        Ok(true)
    }

    /// Puts `FORMAT` back to the features of the sealed log and of the open held batch, after a
    /// batch that published more failed and was rolled back (D-WWAK7: the sealed log is the authority, and a failed
    /// first use never needs a newer rdy). Best effort: a `FORMAT` left ahead is the crash
    /// window's leftover, which the next open puts back.
    fn restore_mirror(&self) {
        let features = self.held_features.as_ref().unwrap_or(&self.features);
        if format::read_format_file(&self.dir)
            .ok()
            .flatten()
            .is_none_or(|m| m != *features)
        {
            let _ = format::write_format_file(&self.dir, features);
        }
    }

    /// `items` with what the store already holds left out, and the hash of every item.
    fn dedupe(&self, items: Vec<(u8, Vec<u8>)>) -> Result<(Vec<Hash>, Frames), StoreError> {
        let mut hashes = Vec::with_capacity(items.len());
        let mut batch = Vec::with_capacity(items.len());
        let mut seen = BTreeSet::new();
        for (kind, payload) in items {
            let hash = Hash::of(&payload);
            hashes.push(hash);
            // kind1: dedupe by (kind, hash); the same bytes under another kind are another
            // object. A ref update or a transition record is never deduped: two batches may log
            // the same bytes.
            let logical = crate::codec::logical_kind(kind);
            if logical != kinds::REF_UPDATE
                && logical != kinds::TRANSITION
                && (seen.contains(&(logical, hash))
                    || self.index().typed(logical, &hash)?.is_some())
            {
                continue;
            }
            seen.insert((logical, hash));
            batch.push((kind, payload));
        }
        Ok((hashes, batch))
    }

    /// A write of a held batch: its frames join the open batch (the first write of a batch
    /// opens it, and may roll the segment), with no marker and no sync, and are indexed at once
    /// with a commit that is not durable. A redaction apply's frame is indexed as an object, but
    /// its mask is activated only by the seal (`seal_held_apply`), after the batch is synced.
    fn append_held(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        let (hashes, batch) = self.dedupe(items)?;
        if batch.is_empty() {
            if self.open.is_none() {
                self.last_batch.clear();
                self.last_batch_end = self.log.end();
            }
            return Ok(hashes);
        }
        let placed = self.log.append_frames(&batch, self.open.is_none())?;
        // an apply's mask is not activated here: the shared index serves every reader, and the
        // apply authorizes nothing before its seal syncs it (`seal_held_apply` activates it then)
        let entries: Vec<IndexEntry> = self
            .placed_entries(&batch, &placed)?
            .into_iter()
            .filter(|e| !matches!(e, IndexEntry::RedactionApply { .. }))
            .collect();
        if self.take_index_fault() {
            return Err(StoreError::Db("injected index failure".into()));
        }
        self.index().apply(&entries, self.log.end())?;
        let open = self.open.get_or_insert(OpenBatch {
            start: placed[0].0,
            frames: vec![],
        });
        open.frames.extend(
            batch
                .iter()
                .zip(&placed)
                .map(|((k, _), (_, h))| (crate::codec::logical_kind(*k), *h)),
        );
        Ok(hashes)
    }

    fn append_and_index(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        // an apply seals only through `seal_apply_batch`, whose failures after the sync never
        // reach this path's rollback
        if items
            .iter()
            .any(|(k, _)| crate::codec::logical_kind(*k) == kinds::REDACTION_APPLY)
        {
            return Err(StoreError::Refused(
                "a redaction apply seals only through the checked apply writer".into(),
            ));
        }
        let (hashes, batch) = self.dedupe(items)?;
        if batch.is_empty() {
            self.last_batch.clear();
            self.last_batch_end = self.log.end();
            return Ok(hashes);
        }
        let placed = self.log.append_batch(&batch)?;
        let entries = self.placed_entries(&batch, &placed)?;
        self.log.crash_point(CrashPoint::BeforeSync);
        self.log.sync()?;
        self.synced_unindexed = true;
        self.log.crash_point(CrashPoint::AfterSync);
        if self.take_index_fault() {
            return Err(StoreError::Db("injected index failure".into()));
        }
        self.index().apply(&entries, self.log.end())?;
        self.synced_unindexed = false;
        self.log.crash_point(CrashPoint::AfterIndex);
        self.acknowledge(
            batch
                .iter()
                .zip(&placed)
                .map(|((kind, _), (_, hash))| (*kind, *hash))
                .collect(),
        );
        Ok(hashes)
    }

    /// Makes the index durable up to everything written so far: one durable index commit. Until
    /// then a kill or power loss leaves the durable index behind the log, which the next open
    /// catches up from the log.
    pub fn checkpoint(&mut self) -> Result<(), StoreError> {
        if self.open.is_some() {
            return Err(batch_open("before a checkpoint"));
        }
        if self.stale_index {
            return Err(poisoned_error());
        }
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
        if self.open.is_some()
            || self.pending == 0
            || now.saturating_duration_since(self.last_write) < CHECKPOINT_IDLE
        {
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
        if self.log.is_read_only() {
            // nothing to seal, checkpoint or remove: the private index goes with its last reader
            drop(self.drop_index());
            return Ok(());
        }
        // an open held batch is sealed first, so a write left open is kept
        let sealed = self.seal(vec![]);
        if self.open.is_some() || self.stale_index {
            // The index holds entries of frames the log does not keep (a failed seal, or a batch
            // left open on a poisoned store), and a clean close of the database makes them
            // durable. So the file is removed first, and the removal is made durable; only then
            // does the database close, onto a file no name reaches. The next open rebuilds the
            // index from the log; the sentinel stays. If the removal fails, the database is never
            // closed cleanly: the next open then sees a kill, which drops those entries.
            let removed = remove_if_present(&self.dir.join("index.redb"))
                .and_then(|()| fsutil::sync_dir(&self.dir).map_err(Into::into));
            if let Err(e) = removed {
                std::mem::forget(self.reader.take());
                return Err(e);
            }
            let err = self.poisoned_err();
            drop(self.drop_index());
            self.log.crash_point(CrashPoint::StaleIndexClosed);
            sealed?;
            return Err(err);
        }
        sealed?;
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
        self._lock.0.0.try_clone()
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

    /// Test hook: the next log sync fails, after the frames before it (and a batch's marker)
    /// were written.
    #[doc(hidden)]
    pub fn fail_next_sync(&mut self) {
        self.log.fail_next_sync();
    }

    /// Test hook: the next appended frame is written only in part, then the write fails.
    #[doc(hidden)]
    pub fn fail_next_append_after(&mut self, bytes: usize) {
        self.log.fail_next_append_after(bytes);
    }

    /// Test hook: the appends write `frames` frames whole, then the next one only in part
    /// (`bytes` bytes), and that write fails; `None` disarms it.
    #[doc(hidden)]
    pub fn fail_append_at_frame_for_tests(&mut self, at: Option<(usize, usize)>) {
        self.log.fail_append_at_frame(at);
    }

    /// Writes `value` as one object of `kind` (see `write`). A block or change that is a read
    /// projection is refused before it is serialized: `Redacted` when its marker names a request
    /// that removed it, `ProjectedWrite` for a forged or unmatched marker.
    ///
    /// A `BLOCK` takes a `Block`, and a `CHANGE` takes a `Change`, bare, boxed or as a `'static`
    /// reference. Any other value of those kinds is `Refused`, because its read identity cannot
    /// be checked and serialization would drop it.
    pub fn put<T: Serialize + 'static>(&mut self, kind: u8, value: &T) -> Result<Hash, StoreError> {
        let any: &dyn std::any::Any = value;
        match typed_record(any) {
            TypedRecord::Block(block) => {
                if kind == kinds::CHANGE {
                    return Err(unchecked_put(kind));
                }
                self.refuse_block_projection(block)?;
            }
            TypedRecord::Change(change) => {
                if kind == kinds::BLOCK {
                    return Err(unchecked_put(kind));
                }
                self.refuse_projection(change)?;
            }
            TypedRecord::Other if kind == kinds::BLOCK || kind == kinds::CHANGE => {
                return Err(unchecked_put(kind));
            }
            TypedRecord::Other => {}
        }
        Ok(self.write(vec![crate::codec::item(kind, value)?])?[0])
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
    /// writes one, with its review (FW-CV4). A `pair/` or `semantic/` ref is refused: only
    /// `put_verdict` and `put_tension_versions` write them, with their objects (plan M2e1). An
    /// `inbox/` ref is refused: only `put_inbox` writes one, after its checks (plan M2e2). An
    /// `answer/` or `pending/` ref is refused: only `put_answer` writes one, with its answer
    /// (plan Decision intake). A `meta/` ref is refused: only the store writes one (plan Export
    /// and clone, `crate::meta`).
    ///
    /// The mode and holder ref rules (`crate::holder`) come before those refusals, so their exact
    /// errors are the same here as through `write` and the seals: `meta/store_mode` is `store
    /// mode cannot change`, and a ref a holder never holds is `a holder store cannot write ref`.
    pub fn set_refs(&mut self, refs: Vec<(String, Vec<u8>)>) -> Result<(), StoreError> {
        let items = self.ref_items(refs.clone());
        // the mode and holder rules first; a `meta/` ref is refused below, before `write`
        self.check_mode_refs(&items, RefWrite::Public)?;
        // a `plan/` ref: only the plan writers write one, with its plan
        crate::plan::refuse_plan_writes(&items)?;
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
        if let Some((name, _)) = refs
            .iter()
            .find(|(name, _)| name.starts_with(crate::review::PAIR_REF))
        {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is a verdict: only put_verdict writes one"
            )));
        }
        if let Some((name, _)) = refs
            .iter()
            .find(|(name, _)| name.starts_with(crate::review::SEMANTIC_REF))
        {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is a semantic tension: only put_verdict and put_tension_versions write one"
            )));
        }
        if let Some((name, _)) = refs
            .iter()
            .find(|(name, _)| name.starts_with(crate::inbox::INBOX_REF))
        {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is an inbox: only put_inbox writes one"
            )));
        }
        if let Some((name, _)) = refs.iter().find(|(name, _)| {
            name.starts_with(crate::answer::ANSWER_REF)
                || name.starts_with(crate::answer::PENDING_REF)
        }) {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is an answer: only put_answer writes one"
            )));
        }
        if let Some((name, _)) = refs
            .iter()
            .find(|(name, _)| name.starts_with(crate::meta::META_REF))
        {
            return Err(StoreError::Decode(format!(
                "ref {name:?} is project metadata: only the store writes meta/ refs"
            )));
        }
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
        self.write_refs(items, RefWrite::Unchecked)?;
        Ok(())
    }

    /// The hash `put_blob` would return for these bytes, without storing anything.
    pub fn blob_hash(bytes: &[u8]) -> Hash {
        let chunks: Vec<Hash> = bytes
            .chunks(CHUNK)
            .map(|c| Hash::of(&crate::codec::frame_latest(kinds::BLOB_CHUNK, c.to_vec()).1))
            .collect();
        crate::codec::encode_new(
            kinds::BLOB_MANIFEST,
            &BlobManifest {
                len: bytes.len() as u64,
                chunks,
            },
        )
        .hash()
    }

    pub fn put_blob(&mut self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let mut items: Vec<(u8, Vec<u8>)> = bytes
            .chunks(CHUNK)
            .map(|c| crate::codec::frame_latest(kinds::BLOB_CHUNK, c.to_vec()))
            .collect();
        let chunks: Vec<Hash> = items.iter().map(|(_, c)| Hash::of(c)).collect();
        items.push(crate::codec::item_new(
            kinds::BLOB_MANIFEST,
            &BlobManifest {
                len: bytes.len() as u64,
                chunks,
            },
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
    /// What the open of this reader's store permits; a reader keeps it after its store closes.
    pub fn access(&self) -> format::StoreAccess {
        match self.shared.access {
            Access::ReadWrite => format::StoreAccess::ReadWrite,
            Access::ReadOnly { .. } => format::StoreAccess::ReadOnly,
        }
    }

    /// Test hook: the private index directory of a read-only store's readers.
    #[doc(hidden)]
    pub fn private_index_dir_for_tests(&self) -> Option<PathBuf> {
        self.shared
            ._hold
            .as_ref()
            .map(|h| h.dir.path().to_path_buf())
    }

    /// `ReadOnly` (E_STORE_READ_ONLY) when this reader's store permits reads only.
    pub fn ensure_writable(&self) -> Result<(), StoreError> {
        match &self.shared.access {
            Access::ReadWrite => Ok(()),
            Access::ReadOnly {
                feature,
                required_rdy,
            } => Err(StoreError::ReadOnly {
                feature: feature.clone(),
                required_rdy: required_rdy.clone(),
            }),
        }
    }

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

    /// The segments this reader reads by position.
    pub(crate) fn segments(&self) -> &SegmentReader {
        &self.shared.segments
    }

    /// The preview overlay of this reader, if it is a preview reader.
    pub(crate) fn preview(&self) -> Option<&crate::redaction::Preview> {
        self.preview.as_deref()
    }

    /// The redaction epoch of the reader family now (an Acquire load).
    pub fn redaction_epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::Acquire)
    }

    /// The redaction epoch this reader captured when it was taken.
    pub fn captured_redaction_epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether this reader may serve a new response: its captured epoch is the family's
    /// (`RedactedState` when a redaction was applied since it was taken), and the store is not
    /// poisoned (the store's poisoned-handle error).
    pub fn check_redaction_epoch(&self) -> Result<(), StoreError> {
        self.refuse_pending()?;
        if self.epoch != self.redaction_epoch() {
            return Err(StoreError::RedactedState);
        }
        if self.shared.poisoned.load(Ordering::Acquire) {
            return Err(poisoned_error());
        }
        Ok(())
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
    ///
    /// A frame whose payload an applied redaction removed stops the walk with `Redacted`: this
    /// walk hands out only original payloads. `for_each_record_from` walks past such frames.
    pub fn for_each_frame_from(
        &self,
        from: Location,
        upto: Location,
        mut f: impl FnMut(Location, u8, Hash, &[u8], Location) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.refuse_pending()?;
        let spans = self.index().redaction_spans_in(from, upto)?;
        if spans.is_empty() {
            return self.shared.segments.for_each_from(from, upto, f);
        }
        let holes = holes_of(&spans);
        self.shared
            .segments
            .for_each_view_from(from, upto, &holes, |loc, v, next| {
                if let Some(s) = spans.get(&loc) {
                    return Err(StoreError::Redacted { request: s.request });
                }
                match v.payload {
                    Some(payload) => f(loc, v.kind, v.hash, payload, next),
                    None => Err(StoreError::corrupt_at(loc)),
                }
            })
    }

    /// The first object written with hash `h`, whatever its kind, and that kind. A payload a
    /// redaction removed is `Redacted`, never a valid object.
    pub fn get_raw(&self, h: &Hash) -> Result<(u8, Vec<u8>), StoreError> {
        self.refuse_pending()?;
        let loc = self.index().object(h)?.ok_or(StoreError::NotFound(*h))?;
        if let Some(request) = self.mask_at(&loc)? {
            return Err(StoreError::Redacted { request });
        }
        self.read_object(h, loc)
    }

    /// Reads the frame at `loc`, which the index says holds an object with hash `h`.
    fn read_object(&self, h: &Hash, loc: Location) -> Result<(u8, Vec<u8>), StoreError> {
        self.refuse_pending()?;
        self.shared.frames_read.fetch_add(1, Ordering::Relaxed);
        let (kind, found, payload) = self.shared.segments.read_hashed(loc)?;
        if found != *h {
            return Err(StoreError::Decode(format!(
                "the index places {h} at {loc:?}, which holds {found}; run rdy reindex"
            )));
        }
        Ok((kind, payload))
    }

    /// The body of the object of `kind` with hash `h` (kind1), in whatever encoding it was
    /// written ([`StoreReader::get_body`]). A payload a redaction removed is `Redacted`.
    pub fn get_kind(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError> {
        self.get_body(kind, h)
    }

    /// [`StoreReader::get_kind`] with the frame's raw kind: the stored payload of the object of
    /// logical kind `kind` with hash `h`, as `crate::codec` splits it.
    pub fn get_kind_raw(&self, kind: u8, h: &Hash) -> Result<(u8, Vec<u8>), StoreError> {
        self.refuse_pending()?;
        let loc = self
            .index()
            .typed(kind, h)?
            .ok_or(StoreError::NotFound(*h))?;
        if let Some(request) = self.mask_at(&loc)? {
            return Err(StoreError::Redacted { request });
        }
        let (raw, payload) = self.read_object(h, loc)?;
        let found_kind = crate::codec::logical_kind(raw);
        if found_kind != kind {
            return Err(StoreError::Decode(format!(
                "the index places a kind {kind} object {h} at {loc:?}, which holds kind {found_kind}; run rdy reindex"
            )));
        }
        Ok((raw, payload))
    }

    /// The body of the object of logical kind `kind` with hash `h`: its stored payload less a
    /// versioned record's version (`crate::codec`), for a reader that decodes it as the current
    /// type.
    pub fn get_body(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError> {
        Ok(self.get_versioned_body(kind, h)?.1)
    }

    /// [`StoreReader::get_body`] with the frame's encoding version: what a reader needs to decode
    /// the body of a kind whose encodings differ in shape (`crate::codec::decode_bounded_split`).
    pub fn get_versioned_body(&self, kind: u8, h: &Hash) -> Result<(u16, Vec<u8>), StoreError> {
        let (raw, mut payload) = self.get_kind_raw(kind, h)?;
        let o =
            crate::codec::split(raw, &payload).map_err(|e| StoreError::Decode(e.to_string()))?;
        let (version, at) = (o.version, payload.len() - o.body.len());
        payload.drain(..at);
        Ok((version, payload))
    }

    /// The object of `kind` with hash `h`, decoded through its encoding (`crate::codec`).
    pub fn get_typed<T: DeserializeOwned + 'static>(
        &self,
        kind: u8,
        h: &Hash,
    ) -> Result<T, StoreError> {
        let (raw, payload) = self.get_kind_raw(kind, h)?;
        crate::codec::decode_record(raw, &payload)
    }

    /// The object of `kind` with hash `h`, decoded; an object of another kind with that hash is
    /// named as what it is (`{h} is not a {what} (object kind {k})`), never decoded as `T`.
    pub(crate) fn get_named<T: DeserializeOwned + 'static>(
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

    /// The change with id `h` (repr1). A change a redaction removed reads as its projection: its
    /// residue, with its original id, audit fields and op order, each op's block read by its
    /// original version (the block, or its stub).
    pub fn change(&self, h: &Hash) -> Result<Change, StoreError> {
        match self.get_named(kinds::CHANGE, "change", h) {
            Err(StoreError::Redacted { request }) => self.change_projection(h, request),
            r => r,
        }
    }

    /// The proposal version with id `h` (repr1).
    pub fn proposal(&self, h: &Hash) -> Result<Proposal, StoreError> {
        self.get_named(kinds::PROPOSAL, "proposal", h)
    }

    /// The landing with id `h` (repr1).
    pub fn landing(&self, h: &Hash) -> Result<LandingRecord, StoreError> {
        self.get_named(kinds::LANDING, "landing", h)
    }

    /// Where the log holds landing `h`: an index lookup that reads no frame. A landing is written
    /// after the landing it follows (its `prev`), and dump, load and reindex keep the log order,
    /// so these order canon's landings as the chain does. Two landings never share one; the
    /// clock plays no part.
    pub fn landing_location(&self, h: &Hash) -> Result<Location, StoreError> {
        self.index()
            .typed(kinds::LANDING, h)?
            .ok_or(StoreError::NotFound(*h))
    }

    /// Where the log holds change `h`: an index lookup that reads no frame. Dump, load and
    /// reindex keep the log order, so these order changes as they were written.
    pub fn change_location(&self, h: &Hash) -> Result<Location, StoreError> {
        self.index()
            .typed(kinds::CHANGE, h)?
            .ok_or(StoreError::NotFound(*h))
    }

    /// Whether an object of `kind` with hash `h` is stored: an index lookup that reads no frame.
    pub fn has_kind(&self, kind: u8, h: &Hash) -> Result<bool, StoreError> {
        Ok(self.index().typed(kind, h)?.is_some())
    }

    /// Whether the log holds an object of `kind` with hash `h` before `end` (a published
    /// state's log end): [`StoreReader::has_kind`] less a frame of a batch that is still open or
    /// was sealed after that state. An index lookup that reads no frame.
    pub fn has_kind_before(&self, kind: u8, h: &Hash, end: Location) -> Result<bool, StoreError> {
        Ok(self.index().typed(kind, h)?.is_some_and(|at| at < end))
    }

    /// Log frames read so far by `visit_carried`.
    #[doc(hidden)]
    pub fn carried_frames_read(&self) -> u64 {
        self.shared.carried_frames_read.load(Ordering::Relaxed)
    }

    /// Counts one log frame read by `visit_carried`.
    pub(crate) fn note_carried_frame_read(&self) {
        self.shared
            .carried_frames_read
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Object frames read so far by `get_raw` and `get_kind`.
    #[doc(hidden)]
    pub fn frames_read(&self) -> u64 {
        self.shared.frames_read.load(Ordering::Relaxed)
    }

    pub fn get<T: DeserializeOwned + 'static>(&self, h: &Hash) -> Result<T, StoreError> {
        let (raw, payload) = self.get_raw(h)?;
        crate::codec::decode_record(raw, &payload)
    }

    pub fn has(&self, h: &Hash) -> Result<bool, StoreError> {
        Ok(self.index().object(h)?.is_some())
    }

    /// The block version `version`. A version a redaction removed reads as its stub: id, type,
    /// parent, order and the recorded identity, body `Content removed.`.
    ///
    /// The `BLOCK` frame is read and masked by kind, so an earlier object of another kind with
    /// the same bytes never stands in for a removed version. With no `BLOCK` frame of that hash,
    /// the first object with it is decoded, as before redaction existed.
    pub fn get_block(&self, version: &Hash) -> Result<Block, StoreError> {
        let read = match self.get_kind_raw(kinds::BLOCK, version) {
            // the first object with that hash, read as a block's encoding-zero body, as before
            // redaction existed
            Err(StoreError::NotFound(_)) => self
                .get_raw(version)
                .map(|(_, payload)| (kinds::BLOCK, payload)),
            r => r,
        };
        match read {
            Ok((raw, payload)) => crate::codec::decode_record(raw, &payload),
            Err(StoreError::Redacted { request }) => self.block_projection(version, request),
            Err(e) => Err(e),
        }
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.index().get_ref(name)
    }

    pub fn refs_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let refs = self.index().refs_with_prefix(prefix)?;
        self.shared
            .refs_read
            .fetch_add(refs.len() as u64, Ordering::Relaxed);
        Ok(refs)
    }

    /// The `open/<view>` refs that name an open proposal, with their values (see
    /// `Index::open_proposal_refs`): what `refs_with_prefix("open/")` gives less the empty refs
    /// of the views whose proposal landed.
    pub fn open_proposal_refs(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let refs = self.index().open_proposal_refs()?;
        self.shared
            .refs_read
            .fetch_add(refs.len() as u64, Ordering::Relaxed);
        Ok(refs)
    }

    /// Ref entries read so far by `refs_with_prefix` and `open_proposal_refs`.
    #[doc(hidden)]
    pub fn refs_read(&self) -> u64 {
        self.shared.refs_read.load(Ordering::Relaxed)
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

    /// The state of blob `h` from its manifest and the redaction masks alone: no chunk is read.
    /// A manifest that is not what `put_blob` writes is the byte read's own error.
    pub fn blob_state(&self, h: &Hash) -> Result<BlobState, StoreError> {
        if !self.has_blob(h)? {
            return Ok(BlobState::Missing);
        }
        match self.blob_manifest(h) {
            Ok(_) => Ok(BlobState::Readable),
            Err(StoreError::Redacted { request }) => Ok(BlobState::Removed(request)),
            Err(e) => Err(e),
        }
    }

    pub fn get_blob(&self, h: &Hash) -> Result<Vec<u8>, StoreError> {
        let mut out = Vec::new();
        self.read_blob(h, &mut |chunk| out.extend_from_slice(chunk))?;
        Ok(out)
    }

    /// Checks blob `h` by the rules a read keeps (`read_blob`), keeping no chunk: true when every
    /// rule holds, false when an authorized removal masks the manifest or a chunk (the removal's
    /// own checks cover it). Any other failure is the read's.
    ///
    /// `lens` holds the length of each chunk already read, across every blob one check reaches:
    /// a chunk is read once, however many positions or manifests list it, and each position is
    /// checked against its length.
    pub(crate) fn check_blob(
        &self,
        h: &Hash,
        lens: &mut HashMap<Hash, u64>,
    ) -> Result<bool, StoreError> {
        let m = match self.blob_manifest(h) {
            Ok(m) => m,
            Err(StoreError::Redacted { .. }) => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut left = m.len;
        for c in &m.chunks {
            let len = match lens.get(c) {
                Some(&len) => len,
                None => {
                    let len = self.blob_chunk(c)?.len() as u64;
                    lens.insert(*c, len);
                    len
                }
            };
            left = check_chunk_len(h, left, len)?;
        }
        Ok(true)
    }

    /// Validates the manifest-kind object at `h` against what `put_blob` writes, then hands each
    /// chunk to `sink` in order. Nothing is read before the manifest is known to be canonical.
    fn read_blob(&self, h: &Hash, sink: &mut dyn FnMut(&[u8])) -> Result<(), StoreError> {
        let m = self.blob_manifest(h)?;
        let mut left = m.len;
        for c in &m.chunks {
            let bytes = self.blob_chunk(c)?;
            left = check_chunk_len(h, left, bytes.len() as u64)?;
            sink(&bytes);
        }
        Ok(())
    }

    /// The manifest of blob `h`, checked as `put_blob` writes it: a manifest-kind object, its own
    /// canonical encoding, with no masked manifest or chunk, and the chunk count its length
    /// gives. No chunk is read.
    fn blob_manifest(&self, h: &Hash) -> Result<BlobManifest, StoreError> {
        let (raw, payload) = match self.get_kind_raw(kinds::BLOB_MANIFEST, h) {
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
        let o =
            crate::codec::split(raw, &payload).map_err(|e| StoreError::Decode(e.to_string()))?;
        let m: BlobManifest = crate::codec::decode_split(&o)?;
        // postcard ignores trailing bytes; a manifest that is not its own canonical encoding
        // is not what `put_blob` wrote, and would not re-import under its own name
        if encode(&m) != o.body {
            return Err(StoreError::Decode(format!(
                "{h} is not a canonical blob manifest"
            )));
        }
        // a blob a redaction selected, or one with a removed chunk, is unavailable
        if let Some(e) = self.blob_mask(h, &m)? {
            return Err(e);
        }
        // the chunk count `put_blob` would produce, checked before anything is allocated or read
        if m.chunks.len() as u64 != m.len.div_ceil(CHUNK as u64) {
            return Err(StoreError::Decode(format!(
                "blob {h} lists {} chunks for {} bytes",
                m.chunks.len(),
                m.len
            )));
        }
        Ok(m)
    }

    /// The bytes of chunk `c`: the chunk-kind object, or failing that any object with the
    /// chunk's hash. A chunk written before kind1 may have deduped into a block or snapshot of
    /// the same bytes, and the hash proves the bytes.
    fn blob_chunk(&self, c: &Hash) -> Result<Vec<u8>, StoreError> {
        match self.get_kind(kinds::BLOB_CHUNK, c) {
            Err(StoreError::NotFound(_)) => Ok(self.get_raw(c)?.1),
            r => r,
        }
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

    /// Of `changes` (producers of one version), the one that last wrote it before `change` did:
    /// of those landed before `change`'s landing, or in that landing and written before
    /// `change`, the one whose landing and then itself was written last. Unlike
    /// [`Self::producer_at`] it names one occurrence of a version that a block took more than
    /// once: the one `change` read as its base. None when `change` has not landed or none of
    /// `changes` landed before it.
    pub fn producer_before(
        &self,
        changes: &[Hash],
        change: &Hash,
    ) -> Result<Option<Hash>, StoreError> {
        let at = |kind: u8, h: &Hash| -> Result<Location, StoreError> {
            self.index().typed(kind, h)?.ok_or(StoreError::NotFound(*h))
        };
        let Some(own) = self.index().landing_of(change)? else {
            return Ok(None);
        };
        let bound = (at(kinds::LANDING, &own)?, at(kinds::CHANGE, change)?);
        let mut last: Option<(Location, Location, Hash)> = None;
        for c in changes {
            if let Some(l) = self.index().landing_of(c)? {
                let key = (at(kinds::LANDING, &l)?, at(kinds::CHANGE, c)?, *c);
                if (key.0, key.1) < bound {
                    last = Some(last.map_or(key, |k| k.max(key)));
                }
            }
        }
        Ok(last.map(|(_, _, c)| c))
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

    /// Calls `f` with the hash of every stored object of `kind` (every BLOCK version, whether a
    /// change produced it or not), once each, in ascending hash order, from the index (no log
    /// walk).
    pub fn for_each_object_of(
        &self,
        kind: u8,
        f: impl FnMut(Hash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.index().for_each_object_of(kind, f)
    }

    /// The changes with an op that deletes `block`, ascending.
    pub fn deleters(&self, block: &BlockId) -> Result<Vec<Hash>, StoreError> {
        self.index().deleters(block)
    }

    /// The TRANSITION record with hash `h` (plan M2e2, D-Z0EMP).
    pub fn transitions(&self, h: &Hash) -> Result<Transitions, StoreError> {
        self.get_typed(kinds::TRANSITION, h)
    }

    /// The open tensions the logged moves fold to: (id, the hash of the record that opened it),
    /// in id order.
    pub fn open_tensions(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.index().open_tensions()
    }

    /// The open suspects the logged moves fold to: (suspect key, the hash of the record that
    /// made it new), in key order.
    pub fn open_suspects(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.index().open_suspects()
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
        // a read projection is never serialized as an original object
        self.refuse_projection(change)?;
        // a stored read is checked against its bytes before this edit of its deps, then becomes
        // a new change: the commit writes it in the latest encoding
        if change.stored_identity().is_some() {
            crate::codec::encode(kinds::CHANGE, &*change)?;
            change.read_identity = None;
        }
        change.deps = self.change_deps_resolving(change, view, conflicts)?;
        let mut items = vec![];
        for op in &change.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                items.push(crate::codec::item_new(kinds::BLOCK, block));
            }
        }
        items.push(crate::codec::item(kinds::CHANGE, change)?);
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

impl StoreReader {
    /// `RedactionPending` once a removal failed after its apply was synced: no reader of this
    /// store family serves content again.
    pub(crate) fn refuse_pending(&self) -> Result<(), StoreError> {
        if self.shared.pending.load(Ordering::Acquire) {
            return Err(StoreError::RedactionPending);
        }
        Ok(())
    }
}

/// The removal of payloads (`crate::redaction_io`): finishing authorized work, the live apply
/// writer, and fsck's counts.
impl Store {
    /// The error of a write on a poisoned store: `RedactionPending` after a removal failed past
    /// its synced apply, else the poisoned-handle error.
    fn poisoned_err(&self) -> StoreError {
        if self.shared.pending.load(Ordering::Acquire) {
            StoreError::RedactionPending
        } else {
            poisoned_error()
        }
    }

    /// A failure after a synced apply: the handle is poisoned for writes and reads, its index
    /// is not checkpointed (the close removes it and the next open rebuilds it from the log), and
    /// the error is `RedactionPending`. Nothing is rolled back: the apply stays in the log, and
    /// the next open finishes the exact authorized operation.
    fn pending_failure(&mut self) -> StoreError {
        self.poison();
        self.shared.pending.store(true, Ordering::Release);
        self.stale_index = true;
        StoreError::RedactionPending
    }

    /// Advances the store family's redaction epoch to `epoch` (one past the current value; the
    /// caller checked it) after an apply batch is synced and before any payload is written. The
    /// store's own reader takes the new epoch; every reader taken before keeps its old one, and
    /// refuses new responses with `RedactedState`.
    fn advance_epoch(&mut self, epoch: u64) {
        self.shared.epoch.store(epoch, Ordering::Release);
        if let Some(r) = self.reader.as_mut() {
            r.epoch = epoch;
        }
    }

    /// Abandons the open held batch without sealing it, as a failed seal does: the log is cut
    /// back to the batch's start, nothing of the batch is acknowledged, and nothing it holds is
    /// authorized (no marker and no sync were written, so no redaction apply in it takes effect
    /// and the epoch does not move). The index already holds entries of the abandoned frames, so
    /// the store is poisoned until reopen; the close removes the index and the next open
    /// rebuilds it from the log. Does nothing when no batch is open.
    ///
    /// The engine calls it when a batch that holds a redaction apply cannot be sealed with the
    /// moves it computed (a preview or move failure before authorization).
    pub fn abandon_open_batch(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        let ahead = self.held_features.take().is_some();
        self.poison();
        self.stale_index = true;
        if self.roll_back(open.start) && ahead {
            self.restore_mirror();
        }
    }

    /// The rollback of a failed batch: the log is cut back to `to`. Returns whether it was; a
    /// rollback that fails poisons the store. Only after a confirmed rollback may a caller put
    /// `FORMAT` back ([`Store::restore_mirror`]): a batch that stays in the log, marked, uses
    /// the features `FORMAT` lists.
    fn roll_back(&mut self, to: Location) -> bool {
        let rolled_back =
            !std::mem::take(&mut self.faults.rollback) && self.log.truncate_to(to).is_ok();
        if !rolled_back {
            self.poison();
        }
        rolled_back
    }

    /// Whether the index's seven redaction tables hold exactly the rows the log's validated records
    /// before `upto` (the index's `indexed_upto`) give. They are derived again, in an index in
    /// memory, from the requests, the validated applies and the validated carried records;
    /// nothing in the index is trusted. Only a log with none of these and an index with no row
    /// skips the derivation.
    fn redaction_index_matches(&self, upto: Location) -> Result<bool, StoreError> {
        let auth = Arc::clone(self.log.authorization());
        let have = self.index().redaction_rows()?;
        if auth.requests.is_empty()
            && auth.applies.is_empty()
            && auth.carried.is_empty()
            && have.iter().all(|t| t.is_empty())
        {
            return Ok(true);
        }
        let want = Index::in_memory()?;
        let entries = auth.index_entries(self.log.readers(), upto)?;
        want.apply(&entries, upto)?;
        Ok(want.redaction_rows()? == have)
    }

    /// Makes the index's redaction rows the ones the log's validated records before `upto` give:
    /// on any difference the index is rebuilt from the log and checked again. Returns whether it
    /// was rebuilt. A difference that remains after the rebuild is corruption
    /// (`RedactionCorrupt` at the redaction record the authorization does not hold), never a
    /// silent rebuild at every open.
    fn ensure_redaction_index(&mut self, upto: Location) -> Result<bool, StoreError> {
        if self.redaction_index_matches(upto)? {
            return Ok(false);
        }
        self.reindex()?;
        let upto = self.index().indexed_upto()?;
        if !self.redaction_index_matches(upto)? {
            return Err(self.unexplained_redaction_rows());
        }
        Ok(true)
    }

    /// The refusal of redaction rows a rebuilt index still holds and the log's authorization
    /// does not give: `RedactionCorrupt` at the first redaction request, apply or carried record
    /// frame of the log that the authorization does not hold (else at its first record, or the
    /// log's start).
    fn unexplained_redaction_rows(&self) -> StoreError {
        let auth = Arc::clone(self.log.authorization());
        let walked = self.log.for_each_view(ORIGIN, |loc, v, _| {
            let held = match v.kind {
                kinds::REDACTION => auth
                    .requests
                    .binary_search_by_key(&loc, |(l, _)| *l)
                    .is_ok(),
                kinds::REDACTION_APPLY => auth.holds_apply(loc),
                kinds::REDACTION_CARRIED => auth.holds_carried(loc),
                _ => true,
            };
            if held {
                Ok(())
            } else {
                Err(StoreError::corrupt_at(loc))
            }
        });
        match walked {
            Err(e) => e,
            Ok(()) => {
                let first = auth
                    .requests
                    .first()
                    .map(|(l, _)| *l)
                    .into_iter()
                    .chain(auth.applies.first().map(|a| a.loc))
                    .chain(auth.carried.first().map(|c| c.at))
                    .min()
                    .unwrap_or(ORIGIN);
                StoreError::corrupt_at(first)
            }
        }
    }

    /// Finishes the removal an open found authorized, before the open returns. A failure is
    /// `RedactionPending` (the open is refused; the next one retries), except a proof the open
    /// finds broken, which stays `RedactionCorrupt`.
    fn finish_redactions_at_open(&mut self) -> Result<(), StoreError> {
        if self.log.authorization().holes.is_empty() {
            return Ok(());
        }
        match self.finish(true) {
            Ok(_) => Ok(()),
            Err(e @ StoreError::RedactionCorrupt { .. }) => Err(e),
            Err(_) => Err(StoreError::RedactionPending),
        }
    }

    /// Finishes every authorized removal this store's log holds: each authorized payload that is
    /// not all zero yet (intact, or partly zeroed by an interrupted removal) is overwritten with
    /// zeros through one fixed buffer, in `(segment, offset)` order; each affected segment is
    /// synced; then the derived index is rebuilt from the surviving data and the residues (no
    /// old index is kept). Work already complete is recognized and left alone, so the call is
    /// safe to repeat. Returns the active redaction's objects, frames and bytes, and the
    /// authorized payloads found intact (and hashed) before this call erased them.
    ///
    /// The open runs it before it returns a store, and the live apply writer runs it after the
    /// apply batch is synced. A failure poisons the store (`RedactionPending`): no write and no
    /// read is served, and the next open finishes the removal.
    pub fn finish_redactions(&mut self) -> Result<RedactionCounts, StoreError> {
        self.ensure_writable()?;
        if self.poisoned {
            return Err(self.poisoned_err());
        }
        if self.open.is_some() {
            return Err(batch_open("first"));
        }
        match self.finish(false) {
            Ok(c) => Ok(c),
            Err(e @ StoreError::RedactionCorrupt { .. }) => Err(e),
            Err(_) => Err(self.pending_failure()),
        }
    }

    /// The work of `finish_redactions`. `at_open`: the open's run, which reaches the
    /// after-apply-sync failpoint before its first zero write (the live writer reaches it right
    /// after its sync).
    fn finish(&mut self, at_open: bool) -> Result<RedactionCounts, StoreError> {
        self.finish_erasing(at_open).map(|(c, _)| c)
    }

    /// `finish`, and whether it erased anything (and so rebuilt the index).
    fn finish_erasing(&mut self, at_open: bool) -> Result<(RedactionCounts, bool), StoreError> {
        let auth = Arc::clone(self.log.authorization());
        let mut counts = auth.counts();
        let mut affected: BTreeSet<u32> = BTreeSet::new();
        for (loc, hole) in &auth.holes {
            let state = self.log.hole_state(*loc, hole)?;
            if !auth.erasable(loc) {
                // a span only a carried record explains is never erased: the bootstrap found it
                // all zero, and anything else now is damage
                if state != Some(true) {
                    return Err(StoreError::corrupt_at(*loc));
                }
                continue;
            }
            match state {
                Some(true) => continue,
                Some(false) => counts.checked_payloads += 1,
                None => {}
            }
            if at_open && affected.is_empty() {
                crate::redaction_io::hit(crate::redaction_io::Failpoint::AfterApplySync)?;
            }
            let span = crate::redaction_io::drop_span(*loc, hole, &auth.spans[loc]);
            self.log.zero_payload(&span)?;
            affected.insert(loc.segment);
        }
        if affected.is_empty() {
            return Ok((counts, false));
        }
        self.log.sync_segments(&affected)?;
        crate::redaction_io::hit(crate::redaction_io::Failpoint::AfterSegmentSync)?;
        self.reindex()?;
        Ok((counts, true))
    }

    /// Reads and hash-checks every frame as `verify` does, reading the authorized holes as frames
    /// whose payloads are neither read nor hashed, and returns the data frames with, when an
    /// active redaction exists, its counts (`checked_payloads`: the intact data frames hashed).
    pub fn verify_report(&self) -> Result<VerifyReport, StoreError> {
        if self.open.is_some() {
            return Err(batch_open("first"));
        }
        let (frames, holes) = self.log.verify_frames_counted()?;
        let auth = self.log.authorization();
        let redaction = auth.is_active().then(|| RedactionCounts {
            checked_payloads: (frames - holes) as u64,
            ..auth.counts()
        });
        Ok(VerifyReport { frames, redaction })
    }

    /// Seals one batch that holds REDACTION_APPLY records, then finishes the removal before it
    /// returns. Kept apart from the ordinary append path:
    ///
    /// 1. Before anything is written, the batch is validated with the log as the open validates
    ///    it (`redaction_io::bootstrap_pending`): a landing no person approved is
    ///    `RedactionHuman`, any other failure `RedactionCorrupt`; the epoch's next value is
    ///    checked (overflow is `RedactionPending`).
    /// 2. The batch is appended with its marker and synced. A failure up to here rolls back as
    ///    an ordinary write does: nothing is acknowledged.
    /// 3. From the sync on the removal is authorized and irreversible: the store family's epoch
    ///    advances once (older readers refuse new responses with `RedactedState`), the batch is
    ///    indexed, and `finish_redactions` erases, syncs and rebuilds the index. A failure here
    ///    never truncates the batch: it poisons the handle (`RedactionPending`) and the next open
    ///    finishes the removal.
    pub(crate) fn seal_apply_batch(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
    ) -> Result<RedactionCounts, StoreError> {
        self.ensure_writable()?;
        if self.poisoned {
            return Err(self.poisoned_err());
        }
        if self.open.is_some() {
            return Err(batch_open("before a redaction apply"));
        }
        if !items
            .iter()
            .any(|(k, _)| crate::codec::logical_kind(*k) == kinds::REDACTION_APPLY)
        {
            return Err(StoreError::Refused(
                "a redaction apply batch holds at least one REDACTION_APPLY".into(),
            ));
        }
        crate::redaction_io::refuse_oversized_records(&items)?;
        self.check_refs(&items, RefWrite::Public)?;
        self.refuse_removed(&items)?;
        self.check_new_signatures(&items)?;
        let (_, batch) = self.dedupe(items)?;
        let start = self.log.batch_start(&batch);
        let auth = crate::redaction_io::bootstrap_pending(self.log.readers(), start, &batch)?;
        let epoch = self
            .shared
            .epoch
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(StoreError::RedactionPending)?;
        let mut probe = Resolver::new(&NoneHeld);
        for (kind, payload) in &batch {
            entries_for_frame(*kind, Hash::of(payload), ORIGIN, payload, &mut probe)?;
        }
        let before = self.log.end();
        let placed = match self.log.append_batch(&batch) {
            Ok(p) => p,
            Err(e) => {
                self.roll_back(before);
                return Err(e.into());
            }
        };
        let placed_entries = match self.placed_entries(&batch, &placed) {
            Ok(e) => e,
            Err(e) => {
                self.roll_back(before);
                return Err(e);
            }
        };
        self.log.crash_point(CrashPoint::BeforeSync);
        if let Err(e) = self.log.sync() {
            self.roll_back(before);
            return Err(e.into());
        }
        // authorized: the apply is durable, and no error below rolls it back
        self.log.set_authorization(Arc::new(auth));
        if crate::redaction_io::hit(crate::redaction_io::Failpoint::AfterApplySync).is_err() {
            return Err(self.pending_failure());
        }
        self.advance_epoch(epoch);
        if self.take_index_fault() || self.index().apply(&placed_entries, self.log.end()).is_err() {
            return Err(self.pending_failure());
        }
        self.writes += 1;
        self.acknowledge(
            batch
                .iter()
                .zip(&placed)
                .map(|((kind, _), (_, hash))| (*kind, *hash))
                .collect(),
        );
        match self.finish(false) {
            Ok(c) => Ok(c),
            Err(_) => Err(self.pending_failure()),
        }
    }

    /// The seal hook of a held batch that holds REDACTION_APPLY records. `last` must be empty or
    /// the one final TRANSITION; anything else is `Refused`. Then the batch (its frames read
    /// back from the log, then `last`) is validated as the open validates a stored one. Both
    /// checks run before anything more is written; a refusal leaves the batch open and nothing
    /// poisoned. Then `last` and the marker are appended and synced; a failure there truncates
    /// the batch and poisons the store, as `seal` does. From the sync on, the removal is
    /// authorized: the epoch advances once, the batch's apply masks (which `append_held` kept
    /// out of the shared index) and `last` are indexed, and `finish_redactions` runs; a failure
    /// poisons the handle with `RedactionPending` and never truncates the batch.
    fn seal_held_apply(&mut self, last: Vec<(u8, Vec<u8>)>) -> Result<RedactionCounts, StoreError> {
        // `land_atomic` checked the applies against the log as it stood then, and their masks
        // are not active yet: a BLOCK, CHANGE or BLOB_CHUNK in the tail could add a copy of the
        // removed content that no span covers. So the tail holds the TRANSITION only.
        if !(last.is_empty()
            || (last.len() == 1 && crate::codec::logical_kind(last[0].0) == kinds::TRANSITION))
        {
            return Err(StoreError::Refused(
                "a held batch that holds a redaction apply seals with nothing or one TRANSITION"
                    .into(),
            ));
        }
        let start = self.open.as_ref().expect("a batch is open").start;
        let mut batch: Vec<(u8, Vec<u8>)> = vec![];
        // the masks of the batch's applies, which `append_held` left out of the shared index
        let mut activation: Vec<IndexEntry> = vec![];
        self.log
            .for_each_from(start, |loc, kind, hash, payload, _| {
                if kind == kinds::REDACTION_APPLY {
                    activation.push(crate::redaction::apply_entry(hash, loc, payload)?);
                }
                batch.push((kind, payload.to_vec()));
                Ok::<(), StoreError>(())
            })?;
        batch.extend(last.iter().cloned());
        crate::redaction_io::refuse_oversized_records(&batch)?;
        let auth = crate::redaction_io::bootstrap_pending(self.log.readers(), start, &batch)?;
        let epoch = self
            .shared
            .epoch
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(StoreError::RedactionPending)?;
        let mut probe = Resolver::new(&NoneHeld);
        for (kind, payload) in &last {
            entries_for_frame(*kind, Hash::of(payload), ORIGIN, payload, &mut probe)?;
        }
        let synced = (|| -> Result<Vec<IndexEntry>, StoreError> {
            let placed = self.log.append_frames(&last, false)?;
            let open = self.open.as_mut().expect("a batch is open");
            open.frames.extend(
                last.iter()
                    .zip(&placed)
                    .map(|((k, _), (_, h))| (crate::codec::logical_kind(*k), *h)),
            );
            let placed_entries = self.placed_entries(&last, &placed)?;
            let open = self.open.as_ref().expect("a batch is open");
            self.log
                .append_marker(open.start, open.frames.iter().map(|(_, h)| h))?;
            self.log.crash_point(CrashPoint::BeforeSync);
            self.log.sync()?;
            Ok(placed_entries)
        })();
        let placed_entries = match synced {
            Ok(e) => {
                activation.extend(e);
                activation
            }
            Err(e) => {
                // not synced: nothing is authorized, and the batch goes as a failed seal's does.
                // FORMAT goes back to the sealed log's only once the batch is gone: a marker
                // written whole may survive a failed sync, and a failed truncate keeps it in the
                // log, so FORMAT keeps the manifest the batch published
                self.open = None;
                self.poison();
                self.stale_index = true;
                let ahead = self.held_features.take().is_some();
                if self.roll_back(start) && ahead {
                    self.restore_mirror();
                }
                return Err(e);
            }
        };
        // authorized: the apply is durable, and no error below rolls it back. The features the
        // batch first used are the store's from here on, whatever fails next
        self.log.set_authorization(Arc::new(auth));
        if let Some(f) = self.held_features.take() {
            self.features = f;
        }
        if crate::redaction_io::hit(crate::redaction_io::Failpoint::AfterApplySync).is_err() {
            self.open = None;
            return Err(self.pending_failure());
        }
        self.advance_epoch(epoch);
        if self.take_index_fault() || self.index().apply(&placed_entries, self.log.end()).is_err() {
            self.open = None;
            return Err(self.pending_failure());
        }
        let open = self.open.take().expect("a batch is open");
        self.acknowledge(open.frames);
        match self.finish(false) {
            Ok(c) => Ok(c),
            Err(_) => Err(self.pending_failure()),
        }
    }

    /// Takes every batch file of one upload into this holder as one sealed batch (hub p-97JVE,
    /// f-NE0Z4; D-BPYN8: closure and tag in one batch). The checks before anything is written
    /// are `crate::take_batch`'s: each file alone, the union over the holder, the root against
    /// `target`, SIG's subject rule, the root's closure, every new removal's portable proof and
    /// every local copy it removes, uncovered embeddings, then `check` (the caller's validation,
    /// once, under this store's lock), then the receiver caps. A refusal writes nothing.
    ///
    /// The batch holds every record the holder lacks, in canonical order (each read again from
    /// its file and checked against what was indexed: changed input is `input changed during
    /// take` and its bytes are rolled back), each approved drop as its original header and zeros,
    /// then the chosen tag's refs, then one marker and one sync. A batch that changes the
    /// holder's removal authorization is checked once more by the holder's own bootstrap before
    /// its sync; from the sync on, it is authorized: the epoch advances for a new apply, every
    /// local copy is erased and the index is rebuilt, and a failure there is `RedactionPending`
    /// (the next open finishes it) and never rolls the batch back. A take with nothing to write
    /// returns the log's end with zero counts and writes no marker.
    pub fn take_batches<E>(
        &mut self,
        files: &[File],
        target: &crate::take_batch::UploadTarget,
        check: impl FnOnce(
            &mut dyn crate::sync_proof::ObjectSource,
            &crate::take_batch::UploadFacts,
        ) -> Result<crate::take_batch::TagChoice, E>,
    ) -> Result<crate::take_batch::BatchTaken, crate::take_batch::TakeError<E>> {
        use crate::take_batch::{Receiver, TakeError};
        crate::take_batch::check_input_bounds(files.len(), target).map_err(TakeError::Stream)?;
        self.ensure_writable().map_err(TakeError::Store)?;
        if self.poisoned {
            return Err(TakeError::Store(self.poisoned_err()));
        }
        if self.held || self.open.is_some() {
            return Err(TakeError::Store(batch_open("before a take")));
        }
        if self.creation.mode != StoreMode::Holder {
            return Err(TakeError::ModeMismatch);
        }
        let repo = self
            .repo_id()?
            .ok_or_else(|| StoreError::Decode("a holder has no meta/repo_id".into()))?;
        let reader = self.reader();
        let auth = Arc::clone(self.log.authorization());
        let plan = crate::take_batch::prepare(
            &Receiver {
                reader: &reader,
                log: self.log.readers(),
                auth: &auth,
                end: self.log.end(),
                repo,
                features: &self.features,
            },
            files,
            target,
            check,
        )?;
        drop(reader);
        self.seal_take(files, plan)
    }

    /// Writes a prepared take (see [`Store::take_batches`]).
    fn seal_take<E>(
        &mut self,
        files: &[File],
        plan: crate::take_batch::Plan,
    ) -> Result<crate::take_batch::BatchTaken, crate::take_batch::TakeError<E>> {
        use crate::take_batch::{BatchTaken, TakeError};
        let taken = |end, frames| BatchTaken {
            new_objects: plan.writes.len() as u64,
            frames,
            redacted_objects: plan.redacted_objects,
            end,
            tag_hash: plan.tag_hash,
            tag: plan.tag.clone(),
        };
        if plan.writes.is_empty() && plan.refs.is_empty() {
            return Ok(BatchTaken {
                redacted_objects: 0,
                ..taken(self.log.end(), 0)
            });
        }
        let refs = self.ref_items(plan.refs.clone());
        // an answer ref may name a TAG object the plan writes in this batch (the root)
        crate::holder::check_ref_writes(self.creation.mode, &refs, RefWrite::Take, |h| {
            let key = crate::object_refs::ObjectKey {
                kind: kinds::TAG,
                hash: *h,
            };
            Ok(plan.writes.iter().any(|w| w.key() == key) || self.has_kind(kinds::TAG, h)?)
        })?;
        crate::holder::refuse_upgrade_writes(&refs, RefWrite::Take)?;
        let epoch = if plan.new_applies {
            Some(
                self.shared
                    .epoch
                    .load(Ordering::Acquire)
                    .checked_add(1)
                    .ok_or(StoreError::RedactionPending)?,
            )
        } else {
            None
        };
        // the features this batch activates are listed by FORMAT before it seals; a failure
        // that rolls the batch back puts FORMAT back (a failed rollback keeps it: the batch may
        // stay in the log, marked)
        let published = match &plan.features {
            Some(f) => self.publish_mirror(f)?,
            None => false,
        };
        let before = self.log.end();
        let appended = self.append_take(files, &plan, &refs);
        let (frames, entries) = match appended {
            Ok(a) => a,
            Err(e) => {
                if self.roll_back(before) && published {
                    self.restore_mirror();
                }
                return Err(e);
            }
        };
        // a batch that changes the removal authorization is judged by the holder's own
        // bootstrap over the log it makes, before it is synced
        let auth = if plan.redaction {
            match crate::redaction_import::bootstrap_holder(self.log.readers()) {
                Ok(a) => Some(a),
                Err(e) => {
                    if self.roll_back(before) && published {
                        self.restore_mirror();
                    }
                    return Err(TakeError::Store(e));
                }
            }
        } else {
            None
        };
        self.log.crash_point(CrashPoint::BeforeSync);
        if let Err(e) = self.log.sync() {
            if self.roll_back(before) && published {
                self.restore_mirror();
            }
            return Err(TakeError::Store(e.into()));
        }
        self.log.crash_point(CrashPoint::AfterSync);
        let n = frames.len() as u64;
        let Some(auth) = auth else {
            let indexed = if self.take_index_fault() {
                Err(StoreError::Db("injected index failure".into()))
            } else {
                self.index().apply(&entries, self.log.end())
            };
            if let Err(e) = indexed {
                // as a failed write: nothing past `before` is acknowledged
                if self.roll_back(before) && published {
                    self.restore_mirror();
                }
                return Err(TakeError::Store(e));
            }
            self.log.crash_point(CrashPoint::AfterIndex);
            if let Some(f) = &plan.features {
                self.features = f.clone();
            }
            self.writes += 1;
            self.acknowledge(frames);
            return Ok(taken(self.log.end(), n));
        };
        // authorized: the batch is durable, and no error below rolls it back
        self.log.set_authorization(Arc::new(auth));
        if let Some(f) = &plan.features {
            self.features = f.clone();
        }
        if crate::redaction_io::hit(crate::redaction_io::Failpoint::AfterApplySync).is_err() {
            return Err(TakeError::Store(self.pending_failure()));
        }
        if let Some(epoch) = epoch {
            self.advance_epoch(epoch);
        }
        let rebuilt = match self.finish_erasing(false) {
            Ok((_, true)) => Ok(()),
            Ok((_, false)) => self.reindex(),
            Err(e) => Err(e),
        };
        if rebuilt.is_err() {
            return Err(TakeError::Store(self.pending_failure()));
        }
        self.writes += 1;
        self.acknowledge(frames);
        Ok(taken(self.log.end(), n))
    }

    /// Appends a prepared take's frames and marker, with no sync: each new record read again
    /// from its file and checked, each drop as its header and zeros, then `refs`. Returns the
    /// kind and hash of every data frame and, for a batch that leaves the authorization as it
    /// is, their index entries. The caller rolls back on error.
    #[allow(clippy::type_complexity)]
    fn append_take<E>(
        &mut self,
        files: &[File],
        plan: &crate::take_batch::Plan,
        refs: &[(u8, Vec<u8>)],
    ) -> Result<(Vec<(u8, Hash)>, Vec<IndexEntry>), crate::take_batch::TakeError<E>> {
        use crate::take_batch::Write;
        let size = plan
            .writes
            .iter()
            .map(|w| HEADER + u64::from(w.len()))
            .chain(refs.iter().map(|(_, p)| HEADER + p.len() as u64))
            .sum::<u64>()
            + HEADER
            + crate::segment::MARKER_MAX;
        self.log.roll_for_batch(size)?;
        let start = self.log.end();
        let mut frames: Vec<(u8, Hash)> = Vec::new();
        frames
            .try_reserve_exact(plan.writes.len() + refs.len())
            .map_err(|_| crate::stream_error::StreamError::allocation(None, 0))?;
        let mut entries = vec![];
        let mut buf = Vec::new();
        let reader = self.reader();
        let mut r = Resolver::new(&reader);
        for (i, w) in plan.writes.iter().enumerate() {
            if let Some(hook) = self.faults.take_append.as_mut() {
                hook(i as u64);
            }
            match w {
                Write::Present { key, at } => {
                    crate::take_batch::reread(files, at, &mut buf)
                        .map_err(|e| e.with_object(Some(*key)))?;
                    // the verified frame as its source stored it: its raw kind, never the
                    // logical kind of its key
                    let (loc, hash) = self.log.append_unrolled(at.kind, &buf)?;
                    if !plan.redaction {
                        entries.extend(entries_for_frame(at.kind, hash, loc, &buf, &mut r)?);
                    }
                    frames.push((at.kind, hash));
                }
                Write::Dropped {
                    key,
                    raw,
                    len,
                    verify,
                } => {
                    crate::take_batch::verify(files, verify, &mut buf)
                        .map_err(|e| e.with_object(Some(*key)))?;
                    // the removed frame's raw kind, as its source stored it
                    self.log.append_dropped(*raw, &key.hash, *len)?;
                    frames.push((*raw, key.hash));
                }
            }
        }
        for (kind, payload) in refs {
            let (loc, hash) = self.log.append_unrolled(*kind, payload)?;
            entries.extend(entries_for_frame(*kind, hash, loc, payload, &mut r)?);
            frames.push((*kind, hash));
        }
        self.log.crash_point(CrashPoint::BeforeMarker);
        self.log
            .append_marker(start, frames.iter().map(|(_, h)| h))?;
        Ok((frames, entries))
    }

    /// Test hook: `hook` runs before a take reads and appends each new record, with the number
    /// of records appended so far.
    #[doc(hidden)]
    pub fn on_take_append_for_tests(&mut self, hook: impl FnMut(u64) + Send + 'static) {
        self.faults.take_append = Some(Box::new(hook));
    }

    /// Test hook: writes `items` as the store's own checked writers do (any kind, a removed hash
    /// refused), so a test can place a record of a redaction kind that no checked writer builds,
    /// such as a malformed one, and read it back through a production path.
    #[doc(hidden)]
    pub fn write_guarded_for_tests(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
    ) -> Result<Vec<Hash>, StoreError> {
        self.write_guarded(items)
    }

    /// Test hook: seals `items`, a landing batch with its REDACTION_APPLY records, through the
    /// checked apply writer (`seal_apply_batch`): the batch is validated exactly as an open
    /// validates a stored one, before anything is written. The store's own landing path
    /// (`land_atomic`) calls that writer after its checks; this hook drives it directly.
    #[doc(hidden)]
    pub fn seal_redaction_batch_for_tests(
        &mut self,
        items: Vec<(u8, Vec<u8>)>,
    ) -> Result<RedactionCounts, StoreError> {
        self.seal_apply_batch(items)
    }

    /// Test hook: forges an active span over the frame at `loc` in the index (an authorization
    /// the log does not hold).
    #[doc(hidden)]
    pub fn forge_active_span_for_tests(
        &mut self,
        loc: Location,
        kind: u8,
        hash: Hash,
        len: u32,
        apply: Hash,
        request: Hash,
    ) -> Result<(), StoreError> {
        let span = crate::index::RedactedSpan {
            loc,
            kind,
            hash,
            len,
            residue: None,
        };
        self.index()
            .forge_active_span_for_tests(&span, &apply, &request)
    }

    /// Test hook: makes the log's validated redaction authorization forget the log's requests
    /// and applies; the authorized holes and their residues stay.
    #[doc(hidden)]
    pub fn forget_redaction_authorization_for_tests(&mut self) {
        let mut auth = (**self.log.authorization()).clone();
        auth.applies.clear();
        auth.requests.clear();
        self.log.set_authorization(Arc::new(auth));
    }

    /// Test hook: the open's check of the index's redaction rows against the log's validated
    /// authorization, from the index's `indexed_upto`.
    #[doc(hidden)]
    pub fn check_redaction_index_for_tests(&mut self) -> Result<(), StoreError> {
        let upto = self.index().indexed_upto()?;
        self.ensure_redaction_index(upto).map(|_| ())
    }

    /// Test hook: empties the index's applied, active and span tables (an index that forgot the
    /// log's authorization).
    #[doc(hidden)]
    pub fn clear_redaction_rows_for_tests(&mut self) -> Result<(), StoreError> {
        self.index().clear_redaction_rows_for_tests()
    }
}

/// A `put` value seen as the block or change it serializes as, if `put` can check it.
enum TypedRecord<'a> {
    Block(&'a Block),
    Change(&'a Change),
    Other,
}

/// The block or change behind `any`: bare, boxed or a `'static` reference. Every wrapper `put`
/// accepts for a `BLOCK` or a `CHANGE` is listed here; a wrapper not listed is `Other`.
fn typed_record(any: &dyn std::any::Any) -> TypedRecord<'_> {
    if let Some(b) = any.downcast_ref::<Block>() {
        TypedRecord::Block(b)
    } else if let Some(b) = any.downcast_ref::<Box<Block>>() {
        TypedRecord::Block(b)
    } else if let Some(b) = any.downcast_ref::<&'static Block>() {
        TypedRecord::Block(b)
    } else if let Some(c) = any.downcast_ref::<Change>() {
        TypedRecord::Change(c)
    } else if let Some(c) = any.downcast_ref::<Box<Change>>() {
        TypedRecord::Change(c)
    } else if let Some(c) = any.downcast_ref::<&'static Change>() {
        TypedRecord::Change(c)
    } else {
        TypedRecord::Other
    }
}

/// The refusal of a `put` value whose read identity cannot be checked for `kind`.
fn unchecked_put(kind: u8) -> StoreError {
    let want = if kind == kinds::BLOCK {
        "Block"
    } else {
        "Change"
    };
    StoreError::Refused(format!(
        "put of kind {kind} takes a {want}, bare, boxed or as a 'static reference: \
         another value's read identity cannot be checked"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{FeatureClass, StoreFeature, StoreFeatures, UpgradeReceipt};

    /// ok1: the upgrade transaction's commit writes exactly the features and receipt refs in one
    /// unheld batch, after the receipt's own checks, and returns that batch's marker hash.
    #[test]
    fn ok1_upgrade_refs_commit_in_one_checked_batch() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("s");
        let mut s = Store::create(&dir).unwrap();
        let lock = s._lock.clone();
        assert!(
            lock.guards(&dir),
            "the store's own lock guards its directory"
        );
        let to = StoreFeatures::of(vec![StoreFeature {
            name: "test_upgrade".into(),
            class: FeatureClass::Compatible,
            required_rdy: "1.0.0".into(),
        }]);
        let receipt = |from: Hash, to: Hash, end: Location| UpgradeReceipt {
            from,
            to,
            steps: vec!["test_upgrade".into()],
            source_end: end,
            backup: format!(".store-upgrade-{}-{}", "a".repeat(64), "b".repeat(64)),
        };
        let end = s.log_end();
        let from = s.features().hash();
        // each check refuses before anything is appended
        for bad in [
            receipt(
                from,
                to.hash(),
                Location {
                    segment: 0,
                    offset: 1,
                },
            ),
            receipt(Hash([1; 32]), to.hash(), end),
            receipt(from, Hash([2; 32]), end),
            UpgradeReceipt {
                backup: "backup".into(),
                ..receipt(from, to.hash(), end)
            },
        ] {
            assert!(matches!(
                s.commit_upgrade_refs(&to, &bad.to_json()),
                Err(StoreError::Refused(_))
            ));
            assert_eq!(s.log_end(), end, "nothing is appended");
        }
        s.set_held(true).unwrap();
        s.put_blob(b"held").unwrap();
        assert!(
            s.commit_upgrade_refs(&to, b"{}").is_err(),
            "a held batch is open"
        );
        s.set_held(false).unwrap();
        let end = s.log_end();
        let good = receipt(from, to.hash(), end);
        let marker = s.commit_upgrade_refs(&to, &good.to_json()).unwrap();
        let batch: Vec<u8> = s.last_batch().iter().map(|(k, _)| *k).collect();
        assert_eq!(batch, [kinds::REF_UPDATE, kinds::REF_UPDATE]);
        assert_eq!(
            s.get_ref(crate::format::REF_STORE_FEATURES).unwrap(),
            Some(to.to_json())
        );
        assert_eq!(
            s.get_ref(crate::format::REF_UPGRADE).unwrap(),
            Some(good.to_json())
        );
        assert_eq!(s.features(), &to);
        let mut last = None;
        s.log
            .for_each_from(end, |_, kind, hash, _, _| {
                if kind == kinds::BATCH_COMMIT {
                    last = Some(hash);
                }
                Ok::<(), StoreError>(())
            })
            .unwrap();
        assert_eq!(last, Some(marker), "the batch's marker hash");
        s.reader().check_meta().unwrap();
        drop((s, lock));
        let old = std::fs::read(dir.join(crate::format::FORMAT_FILE)).unwrap();
        // FORMAT still mirrors the old manifest: the mirror is the upgrade's to publish. The
        // upgrade's own open accepts the store, as the receipt explains the difference, and
        // leaves FORMAT as it is.
        let s = Store::open_unupgraded_locked(&dir, &StoreLock::take(&dir).unwrap()).unwrap();
        assert_eq!(s.features(), &to);
        drop(s);
        assert_eq!(
            std::fs::read(dir.join(crate::format::FORMAT_FILE)).unwrap(),
            old
        );
    }

    /// ok1: when the upgrade batch rolls to a new segment, the hash returned is the marker the
    /// log wrote there.
    #[test]
    fn ok1_upgrade_refs_return_the_marker_across_a_segment_roll() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("s");
        let mut s = Store::create(&dir).unwrap();
        s.set_segment_max(1);
        let to = StoreFeatures::of(vec![StoreFeature {
            name: "test_upgrade".into(),
            class: FeatureClass::Compatible,
            required_rdy: "1.0.0".into(),
        }]);
        let end = s.log_end();
        let receipt = UpgradeReceipt {
            from: s.features().hash(),
            to: to.hash(),
            steps: vec!["test_upgrade".into()],
            source_end: end,
            backup: format!(".store-upgrade-{}-{}", "a".repeat(64), "b".repeat(64)),
        };
        let marker = s.commit_upgrade_refs(&to, &receipt.to_json()).unwrap();
        let now = s.log_end();
        assert_eq!(now.segment, end.segment + 1, "the batch rolled");
        let frames = s
            .log
            .scan_from(Location {
                segment: now.segment,
                offset: 0,
            })
            .unwrap();
        let (_, kind, hash) = *frames.last().unwrap();
        assert_eq!(kind, kinds::BATCH_COMMIT);
        assert_eq!(marker, hash, "the marker the log wrote");
    }

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
