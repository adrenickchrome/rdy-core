use crate::error::StoreError;
use crate::format::{self, Format};
use crate::fsutil;
use crate::index::{Index, IndexEntry};
use crate::kinds::{self, BlobManifest, RefUpdate};
use crate::segment::{CrashPoint, Location, SegmentLog, find_gap, log_has_marker, seg_path};
use rdy_model::{Block, Change, Hash, Op};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

const SEGMENT_MAX: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;

/// The largest blob a chunk-kind object may stand for as a manifest: an agent chose that object's
/// bytes, so its `len` is not trusted to size the work of reading it. Matches the largest asset
/// `rdy_asset` accepts.
pub const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

/// What one `Store::open` did, for tests, `rdy` diagnostics and the QA gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// This open used the marker rule (the acknowledged boundary is the last valid batch marker).
    pub marker_rule: bool,
    /// Torn or unacknowledged tails this open moved aside (marker rule).
    pub quarantined: Vec<PathBuf>,
    /// This open upgraded an M1 log.
    pub upgraded: bool,
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

pub struct Store {
    dir: PathBuf,
    report: OpenReport,
    log: SegmentLog,
    index: Index,
    _lock: File,
    faults: Faults,
    /// Set when a failed write could not be rolled back; every later write fails until reopen.
    poisoned: bool,
    /// Directories open fsynced because it created something in them (dur1).
    synced: Vec<PathBuf>,
    /// Opened by `open_legacy_for_tests`: behaves as M1 did (no format, no markers).
    legacy: bool,
}

/// Opens the index, verifies its integrity and reads its acknowledged boundary (None if it has
/// none yet). An index that fails to open, fails the check, or makes redb panic is renamed to
/// `index.redb.damaged-<n>` and replaced by an empty one, which the caller rebuilds from the
/// log; the boundary is then unknown. The flag says whether the index was set aside.
fn open_index(dir: &Path) -> Result<(Index, Option<Location>, bool), StoreError> {
    let path = dir.join("index.redb");
    // redb can panic on a corrupt page instead of returning an error; that is damage too
    let opened = std::panic::catch_unwind(|| {
        Index::open_checked(&path).and_then(|i| {
            let acked = i.acknowledged()?;
            Ok((i, acked))
        })
    })
    .unwrap_or_else(|_| Err(StoreError::Db("index panicked while opening".into())));
    match opened {
        Ok((index, acked)) => Ok((index, acked, false)),
        Err(_) => {
            set_aside(&path)?;
            Ok((Index::open(&path)?, None, true))
        }
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

/// Test-only fault injection (see `fail_next_index_apply`).
#[derive(Default)]
struct Faults {
    index_apply: bool,
    rollback: bool,
}

fn encode<T: Serialize>(v: &T) -> Vec<u8> {
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
    let mut out = vec![IndexEntry::Object(hash, loc)];
    match kind {
        kinds::CHANGE => {
            let change: Change = decode(payload)?;
            for op in &change.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op {
                    out.push(IndexEntry::Producer {
                        version: block.version_hash(),
                        change: hash,
                    });
                }
            }
        }
        kinds::REF_UPDATE => {
            let r: RefUpdate = decode(payload)?;
            out.push(IndexEntry::Ref(r.name, r.value));
        }
        _ => {}
    }
    Ok(out)
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        Store::open_with(dir, false)
    }

    /// Test hook: opens and writes exactly as M1 did, so upgrade tests have an M1 store. No
    /// `FORMAT` is read, checked or written, the M1 open rule is always used, no format batch is
    /// written, and every frame is appended on its own with no batch marker.
    #[doc(hidden)]
    pub fn open_legacy_for_tests(dir: &Path) -> Result<Store, StoreError> {
        Store::open_with(dir, true)
    }

    fn open_with(dir: &Path, legacy: bool) -> Result<Store, StoreError> {
        // dur1: each directory created here is fsynced into its parent before any write is acked
        let mut synced = fsutil::create_dir_durable(dir)?;
        let segments = dir.join("segments");
        synced.extend(fsutil::create_dir_durable(&segments)?);
        let fresh = !dir.join("index.redb").exists() || !dir.join("lock").exists();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        if let Err(e) = lock.try_lock() {
            return Err(lock_error(dir, e)); // lock1
        }
        // a newer store is refused before the index is opened or anything is written
        let format_file = if legacy {
            None
        } else {
            let f = format::read_format_file(dir)?;
            if let Some(f) = f {
                format::check_reader(f, dir)?;
            }
            f
        };
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
        // the index opens first: its indexed_upto is the acknowledged end of the log, which
        // tells the log which torn frames are an unacknowledged tail
        let (index, acked, index_rebuilt) = open_index(dir)?;
        if fresh {
            // dur1: the new lock and index files' entries in the store directory
            fsutil::sync_dir(dir)?;
            synced.push(dir.to_path_buf());
        }
        let mut report = OpenReport {
            index_rebuilt,
            ..OpenReport::default()
        };
        // FORMAT, or a batch marker in the log's last two segments, means an M2 log: it opens by
        // the marker rule. The index's durable end is only the floor under which damage is
        // refused, never the boundary. Without either the log is M1 (or a new store).
        report.marker_rule = !legacy && (format_file.is_some() || log_has_marker(&segments)?);
        let log = if report.marker_rule {
            let (log, quarantined) = SegmentLog::open_marked(&segments, SEGMENT_MAX, acked)?;
            report.quarantined = quarantined;
            log
        } else {
            SegmentLog::open_acked(&segments, SEGMENT_MAX, acked)?
        };
        let mut store = Store {
            dir: dir.to_path_buf(),
            log,
            index,
            _lock: lock,
            faults: Faults::default(),
            poisoned: false,
            synced,
            report,
            legacy,
        };
        let (upto, end) = (store.index.indexed_upto()?, store.log.end());
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
        let frames = store.catch_up()?;
        store.report.caught_up_frames = frames;
        store.report.catch_up_commits = u32::from(frames > 0);
        if !legacy {
            store.ensure_format(format_file.is_some())?;
        }
        Ok(store)
    }

    /// Makes the log hold the store's format and `FORMAT` mirror it. The log's refs are the truth:
    /// a newer `min_reader` there is refused; absent refs, or any refs in a log with no batch marker yet (an M1-rule open), are written as one
    /// marked batch (which also upgrades an M1 log) and then `FORMAT` is written; a missing `FORMAT` is rewritten from
    /// the refs. Both steps are idempotent, so a crash between them is finished by the next open.
    fn ensure_format(&mut self, has_format_file: bool) -> Result<(), StoreError> {
        let read = |this: &Store, name: &str| -> Result<Option<u32>, StoreError> {
            this.index
                .get_ref(name)?
                .map(|v| {
                    format::decode_u32(&v).ok_or_else(|| {
                        StoreError::Decode(format!("the log's ref {name} is not a decimal number"))
                    })
                })
                .transpose()
        };
        let (log_format, min_reader) = (
            read(self, format::REF_LOG_FORMAT)?,
            read(self, format::REF_MIN_READER)?,
        );
        if let Some(min_reader) = min_reader {
            let f = Format {
                log_format: log_format.unwrap_or(format::LOG_FORMAT),
                min_reader,
            };
            format::check_reader(f, &self.dir)?;
        }
        match (log_format, min_reader) {
            // the refs are in the log and the open used the marker rule: only FORMAT is missing
            (Some(log_format), Some(min_reader)) if self.report.marker_rule => {
                if !has_format_file {
                    format::write_format_file(
                        &self.dir,
                        Format {
                            log_format,
                            min_reader,
                        },
                    )?;
                }
            }
            // The refs are absent, or the open used the M1 rule, so the log holds no batch marker
            // yet (a kill or power loss can keep the ref frames and lose the marker). FORMAT
            // selects the marker rule, so a marker must exist before it is written. The refs are
            // idempotent: this batch's marker acknowledges everything before it.
            _ => {
                let held_frames = self.log.end()
                    != Location {
                        segment: 0,
                        offset: 0,
                    };
                self.report.upgraded = held_frames && !self.report.marker_rule;
                let cur = Format::CURRENT;
                self.set_refs(vec![
                    (
                        format::REF_LOG_FORMAT.into(),
                        format::encode_u32(cur.log_format),
                    ),
                    (
                        format::REF_MIN_READER.into(),
                        format::encode_u32(cur.min_reader),
                    ),
                ])?;
                format::write_format_file(&self.dir, cur)?;
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

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Replays the frames past the index's `indexed_upto` into it; returns how many frames it read.
    fn catch_up(&mut self) -> Result<u64, StoreError> {
        let from = self.index.indexed_upto()?;
        let mut entries = vec![];
        let mut frames = 0;
        for (loc, kind, hash) in self.log.scan_from(from)? {
            let (_, payload) = self.log.read(loc)?;
            entries.extend(entries_for_frame(kind, hash, loc, &payload)?);
            frames += 1;
        }
        self.index.apply(&entries, self.log.end())?;
        Ok(frames)
    }

    /// Reads and hash-checks every frame in every segment and checks every batch marker;
    /// returns the number of data frames (markers are not counted).
    pub fn verify(&self) -> Result<usize, StoreError> {
        Ok(self.log.verify_frames()?)
    }

    pub fn reindex(&mut self) -> Result<(), StoreError> {
        let path = self.dir.join("index.redb");
        let tmp = self.dir.join("index.redb.tmp");
        // drop the old database before deleting its file
        self.index = Index::open(&tmp)?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.index = Index::open(&path)?;
        let _ = std::fs::remove_file(&tmp);
        self.catch_up().map(|_| ())
    }

    pub fn write(&mut self, items: Vec<(u8, Vec<u8>)>) -> Result<Vec<Hash>, StoreError> {
        if self.poisoned {
            return Err(StoreError::Io(std::io::Error::other(
                "an earlier failed write could not be rolled back; reopen the store",
            )));
        }
        let start = self.log.end();
        let result = self.append_and_index(items);
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
            if kind != kinds::REF_UPDATE
                && (seen.contains(&hash) || self.index.object(&hash)?.is_some())
            {
                continue;
            }
            seen.insert(hash);
            batch.push((kind, payload));
        }
        if batch.is_empty() {
            return Ok(hashes);
        }
        if self.legacy {
            return self.append_and_index_legacy(hashes, batch);
        }
        let placed = self.log.append_batch(&batch)?;
        let mut entries = vec![];
        for ((kind, payload), (loc, hash)) in batch.iter().zip(&placed) {
            entries.extend(entries_for_frame(*kind, *hash, *loc, payload)?);
        }
        self.log.crash_point(CrashPoint::BeforeSync);
        self.log.sync()?;
        self.log.crash_point(CrashPoint::AfterSync);
        if std::mem::take(&mut self.faults.index_apply) {
            return Err(StoreError::Db("injected index failure".into()));
        }
        self.index.apply(&entries, self.log.end())?;
        self.log.crash_point(CrashPoint::AfterIndex);
        Ok(hashes)
    }

    /// The M1 write: each frame appended on its own with no marker, one sync, one index commit.
    fn append_and_index_legacy(
        &mut self,
        hashes: Vec<Hash>,
        batch: Vec<(u8, Vec<u8>)>,
    ) -> Result<Vec<Hash>, StoreError> {
        let mut entries = vec![];
        for (kind, payload) in &batch {
            let (loc, hash) = self.log.append(*kind, payload)?;
            entries.extend(entries_for_frame(*kind, hash, loc, payload)?);
        }
        self.log.sync()?;
        if std::mem::take(&mut self.faults.index_apply) {
            return Err(StoreError::Db("injected index failure".into()));
        }
        self.index.apply(&entries, self.log.end())?;
        Ok(hashes)
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

    pub fn get_raw(&self, h: &Hash) -> Result<(u8, Vec<u8>), StoreError> {
        let loc = self.index.object(h)?.ok_or(StoreError::NotFound(*h))?;
        let (kind, found, payload) = self.log.read_hashed(loc)?;
        if found != *h {
            return Err(StoreError::Decode(format!(
                "the index places {h} at {loc:?}, which holds {found}; run rdy reindex"
            )));
        }
        Ok((kind, payload))
    }

    pub fn get<T: DeserializeOwned>(&self, h: &Hash) -> Result<T, StoreError> {
        decode(&self.get_raw(h)?.1)
    }

    pub fn has(&self, h: &Hash) -> Result<bool, StoreError> {
        Ok(self.index.object(h)?.is_some())
    }

    pub fn get_block(&self, version: &Hash) -> Result<Block, StoreError> {
        self.get(version)
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

    pub fn set_refs(&mut self, refs: Vec<(String, Vec<u8>)>) -> Result<(), StoreError> {
        let items = self.ref_items(refs);
        self.write(items)?;
        Ok(())
    }

    pub fn get_ref(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.index.get_ref(name)
    }

    pub fn refs_with_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.index.refs_with_prefix(prefix)
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

    /// Whether `h` names a blob (a manifest object) in this store. An object of chunk kind
    /// counts when it is exactly the manifest `put_blob` would have written for a blob that is
    /// all here: the index dedupes by hash alone, so a manifest whose bytes were first stored as
    /// a chunk is that chunk-kind object. That check reads at most `MAX_BLOB_BYTES`, one chunk at
    /// a time, and keeps none of it.
    pub fn has_blob(&self, h: &Hash) -> Result<bool, StoreError> {
        match self.get_raw(h) {
            Ok((kinds::BLOB_MANIFEST, _)) => Ok(true),
            Ok((kinds::BLOB_CHUNK, _)) => match self.read_blob(h, &mut |_| {}) {
                Ok(()) => Ok(true),
                Err(StoreError::Decode(_) | StoreError::NotFound(_)) => Ok(false),
                Err(e) => Err(e),
            },
            Ok(_) => Ok(false),
            Err(StoreError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn get_blob(&self, h: &Hash) -> Result<Vec<u8>, StoreError> {
        let mut out = Vec::new();
        self.read_blob(h, &mut |chunk| out.extend_from_slice(chunk))?;
        Ok(out)
    }

    /// Validates the manifest at `h` against what `put_blob` writes, then hands each chunk to
    /// `sink` in order. Nothing is read before the manifest is known to be canonical and, for a
    /// chunk-kind object (whose bytes an agent chose), within `MAX_BLOB_BYTES`.
    fn read_blob(&self, h: &Hash, sink: &mut dyn FnMut(&[u8])) -> Result<(), StoreError> {
        let (kind, payload) = self.get_raw(h)?;
        // The kind check is defence in depth; the structural checks below are the guard. A
        // manifest's bytes may have been stored first as a chunk, and writes dedupe by hash
        // alone, so a chunk-kind object that decodes as a manifest is accepted too.
        if kind != kinds::BLOB_MANIFEST && kind != kinds::BLOB_CHUNK {
            return Err(StoreError::Decode(format!(
                "{h} is not a blob (object kind {kind})"
            )));
        }
        let m: BlobManifest = decode(&payload)?;
        // postcard ignores trailing bytes; a manifest that is not its own canonical encoding
        // is not what `put_blob` wrote, and would not re-import under its own name
        if encode(&m) != payload {
            return Err(StoreError::Decode(format!(
                "{h} is not a canonical blob manifest"
            )));
        }
        if kind == kinds::BLOB_CHUNK && m.len > MAX_BLOB_BYTES {
            return Err(StoreError::Decode(format!(
                "{h} claims a blob of {} bytes, over the limit of {MAX_BLOB_BYTES}",
                m.len
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
            // a chunk's stored kind is not checked: writes dedupe by hash alone, so a chunk may
            // share its object with a block or snapshot of the same bytes, and the hash already
            // proves the bytes
            let (_, bytes) = self.get_raw(c)?;
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

    pub fn producer(&self, version: &Hash) -> Result<Option<Hash>, StoreError> {
        self.index.producer(version)
    }

    pub fn commit_change(
        &mut self,
        mut change: Change,
        refs: Vec<(String, Vec<u8>)>,
    ) -> Result<Hash, StoreError> {
        change.deps = self.change_deps(&change)?;
        let mut items = vec![];
        for op in &change.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                items.push((kinds::BLOCK, encode(block)));
            }
        }
        items.push((kinds::CHANGE, encode(&change)));
        let change_pos = items.len() - 1;
        items.extend(self.ref_items(refs));
        Ok(self.write(items)?[change_pos])
    }
}

impl Store {
    pub(crate) fn change_deps(&self, change: &Change) -> Result<Vec<Hash>, StoreError> {
        let mut deps = BTreeSet::new();
        for op in &change.ops {
            if let Some(base) = op.base() {
                deps.insert(
                    self.index
                        .producer(&base)?
                        .ok_or(StoreError::NotFound(base))?,
                );
            }
        }
        Ok(deps.into_iter().collect())
    }

    pub(crate) fn ref_items_pub(&self, refs: Vec<(String, Vec<u8>)>) -> Vec<(u8, Vec<u8>)> {
        self.ref_items(refs)
    }
}
