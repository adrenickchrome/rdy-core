//! The follower mark (D-AA4FD: "The mark: a file `.rdy/store/FOLLOWER`, outside the log, written
//! as a private replace with a directory sync, holds the source's repo id and the chain's last
//! trailer digest."). A store with a mark is a follower: a byte-identical prefix of its source's
//! log, and no normal open or write reaches it.
//!
//! The file is at most [`MAX_MARK_BYTES`] bytes: the line `rdy follower 1`, then one line of JSON
//! `{chain,files}`. `chain` is the existing `ChainEnd`, and it includes the marked end, which
//! tells a replay after a crash from a store at its mark. `files` is the source's project files
//! as sorted, unique `{path,hash}` rows, the hash over the file's content bytes. A holder has no
//! rows. Nothing else is accepted: this is a private machine file, not an editable config.
//!
//! The mark is checked against its log at the saved boundary: the repo, the end, the batch marker
//! and the manifest there. A later manifest may differ after an interrupted follow.
//!
//! # Following
//!
//! [`prepare_follow`] checks one increment against the store's verified prefix before any append:
//! its envelope and links against the saved chain, its framing, source places, digests and
//! removals by the chain scanner (`crate::dump_scan`), and any part of it the log already holds
//! against those bytes. Only it makes a [`VerifiedIncrement`], which only
//! [`Store::follow_verified`](crate::Store::follow_verified) spends. The mark and the verified
//! log end identify a replay: there is no journal.
use crate::dump::{self, DumpItem, DumpReader, Frame, MAX_FILES, is_project_file};
use crate::dump_chain::{BatchMarker, ChainEnd, DumpKind};
use crate::dump_scan::{self, Envelope, FrameAction, ProjectFile, ScanError, ScanPlan};
use crate::error::{FollowerMarkFailure as Why, StoreError};
use crate::format::{self, DumpStore, MAX_MANIFEST_BYTES};
use crate::holder::{RefRules, StoreMode};
use crate::ingest::{Ingest, IngestReport};
use crate::kinds::{self, BatchCommit, RefUpdate};
use crate::meta::{REF_REPO_ID, decode_repo_id};
use crate::segment::{HEADER as FRAME_HEADER, Location, SegmentLog};
use crate::store::{Store, StoreLock};
use crate::{fileid, fsutil, pio};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

/// The mark's file name in the store directory.
pub const FOLLOWER_FILE: &str = "FOLLOWER";
/// The mark's first line, with its newline.
const HEADER: &[u8] = b"rdy follower 1\n";
/// The most bytes a mark holds (inclusive).
pub const MAX_MARK_BYTES: u64 = 262_144;
/// The most log frames the saved-boundary check reads (inclusive).
pub const MAX_CHECK_FRAMES: u64 = 100_000_000;

/// One project file of the source: its dump path and the hash of its content bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowerFile {
    pub path: String,
    #[serde(with = "crate::transition::hex::one")]
    pub hash: Hash,
}

/// A follower's mark: where its chain ends, and the source's project files there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowerMark {
    pub chain: ChainEnd,
    pub files: Vec<FollowerFile>,
}

fn refused(why: Why) -> StoreError {
    StoreError::FollowerMark(why)
}

/// `Follower` when the store at `dir` holds a valid mark, the mark's own refusal when it holds a
/// bad one; nothing for a store with none. Reads only. Every ordinary create and open runs it
/// before it plans an upgrade, opens an index or creates a sentinel.
pub fn refuse_follower(dir: &Path) -> Result<(), StoreError> {
    match read_mark(dir)? {
        Some(mark) => Err(StoreError::Follower(mark.chain.repo_id)),
        None => Ok(()),
    }
}

/// The mark of the store at `dir`, or the refusal of an explicit follower open with none.
pub(crate) fn require_mark(dir: &Path) -> Result<FollowerMark, StoreError> {
    read_mark(dir)?.ok_or(refused(Why::Missing))
}

/// The mark of the store at `dir`; None when it has none (a path that is no store has none). The
/// file is opened without following a link, and at most [`MAX_MARK_BYTES`] of it are read.
pub fn read_mark(dir: &Path) -> Result<Option<FollowerMark>, StoreError> {
    let path = dir.join(FOLLOWER_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(_) => return Err(refused(Why::Unreadable)),
    };
    // anything but a regular file (a link, a FIFO, a device) is refused before any open
    if !meta.is_file() {
        return Err(refused(Why::NotPrivate));
    }
    let file = pio::open_no_follow(std::fs::OpenOptions::new().read(true), &path).map_err(|e| {
        refused(
            if pio::is_link_refusal(&e) || e.kind() == std::io::ErrorKind::PermissionDenied {
                Why::NotPrivate
            } else {
                Why::Unreadable
            },
        )
    })?;
    let meta = file.metadata().map_err(|_| refused(Why::Unreadable))?;
    match private_regular(&file, &meta) {
        Ok(true) => {}
        Ok(false) => return Err(refused(Why::NotPrivate)),
        Err(_) => return Err(refused(Why::Unreadable)),
    }
    if meta.len() > MAX_MARK_BYTES {
        return Err(refused(Why::TooLarge));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MARK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused(Why::Unreadable))?;
    if bytes.len() as u64 > MAX_MARK_BYTES {
        return Err(refused(Why::TooLarge));
    }
    parse(&bytes).map(Some)
}

/// Whether the open `file`, whose metadata is `meta`, is one private regular file of this
/// account's: not shared by a second name. Its privacy is read from the handle; a failed read is
/// an error.
fn private_regular(file: &std::fs::File, meta: &std::fs::Metadata) -> std::io::Result<bool> {
    let privacy = pio::privacy(file)?;
    Ok(meta.is_file() && privacy.mine && privacy.private && crate::fileid::links(meta) == 1)
}

/// The mark in `bytes`, which is within the size bound.
fn parse(bytes: &[u8]) -> Result<FollowerMark, StoreError> {
    let Some(rest) = bytes.strip_prefix(HEADER) else {
        return Err(refused(Why::BadHeader));
    };
    // one line: the newline ends it, and there is no third line
    let Some(line) = rest.strip_suffix(b"\n").filter(|l| !l.contains(&b'\n')) else {
        return Err(refused(Why::BadJson));
    };
    let value: Value = serde_json::from_slice(line).map_err(|_| refused(Why::BadJson))?;
    let Some(top) = value.as_object() else {
        return Err(refused(Why::BadJson));
    };
    if top.keys().any(|k| k != "chain" && k != "files") {
        return Err(refused(Why::UnknownField));
    }
    let (Some(chain), Some(files)) = (top.get("chain"), top.get("files")) else {
        return Err(refused(Why::BadJson));
    };
    let chain = parse_chain(chain)?;
    let files = parse_files(files)?;
    Ok(FollowerMark { chain, files })
}

/// The chain end in `value`: the existing codec, with no field it would ignore.
fn parse_chain(value: &Value) -> Result<ChainEnd, StoreError> {
    let chain: ChainEnd = serde_json::from_value(value.clone()).map_err(|e| {
        refused(if e.to_string().contains("unknown field") {
            Why::UnknownField
        } else {
            Why::BadJson
        })
    })?;
    let again = serde_json::to_value(&chain).map_err(|_| refused(Why::BadJson))?;
    if !same_keys(value, &again) {
        return Err(refused(Why::UnknownField));
    }
    if let DumpStore::Features(m) = &chain.store_format {
        let json = m.to_json();
        if json.len() > MAX_MANIFEST_BYTES || format::StoreFeatures::parse(&json).is_err() {
            return Err(refused(Why::BadJson));
        }
    }
    Ok(chain)
}

/// Whether `a` holds no object key that `b` lacks, at any depth.
fn same_keys(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => a
            .iter()
            .all(|(k, v)| b.get(k).is_some_and(|w| same_keys(v, w))),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(v, w)| same_keys(v, w))
        }
        _ => true,
    }
}

/// The project file list in `value`: at most [`MAX_FILES`] rows of the dump's path grammar,
/// sorted by the path's bytes, each path once.
fn parse_files(value: &Value) -> Result<Vec<FollowerFile>, StoreError> {
    let Some(rows) = value.as_array() else {
        return Err(refused(Why::BadFiles));
    };
    if rows.len() > MAX_FILES as usize {
        return Err(refused(Why::BadFiles));
    }
    let mut out: Vec<FollowerFile> = Vec::with_capacity(rows.len());
    for row in rows {
        let file: FollowerFile = serde_json::from_value(row.clone()).map_err(|e| {
            refused(if e.to_string().contains("unknown field") {
                Why::UnknownField
            } else {
                Why::BadFiles
            })
        })?;
        if !is_project_file(&file.path)
            || out
                .last()
                .is_some_and(|p| p.path.as_bytes() >= file.path.as_bytes())
        {
            return Err(refused(Why::BadFiles));
        }
        out.push(file);
    }
    Ok(out)
}

/// The mark's bytes, as the writer makes them: the header, then one compact line of JSON.
fn encode(mark: &FollowerMark) -> Result<Vec<u8>, StoreError> {
    let mut bytes = HEADER.to_vec();
    serde_json::to_writer(&mut bytes, mark).map_err(|_| refused(Why::BadJson))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_MARK_BYTES {
        return Err(refused(Why::TooLarge));
    }
    Ok(bytes)
}

/// Writes `mark` at `dir` through the private durable replace: a failure keeps the old valid mark
/// or the new valid mark, never a part of one and never none.
pub(crate) fn write_mark(dir: &Path, mark: &FollowerMark) -> Result<(), StoreError> {
    let bytes = encode(mark)?;
    fsutil::write_private_durable(&dir.join(FOLLOWER_FILE), &bytes).map_err(StoreError::Io)
}

/// A loader's write of the first mark of its staging store at `dir`, after `Ingest::finish` and
/// before any store opens over it. `lock` is the caller's lock of that store (`Locked` otherwise),
/// the store has no mark yet, and the chain's facts must hold against the sealed log.
pub fn write_seed_mark(
    dir: &Path,
    lock: &StoreLock,
    mark: &FollowerMark,
) -> Result<(), StoreError> {
    if !lock.guards(dir) {
        return Err(StoreError::Locked(dir.to_path_buf()));
    }
    refuse_follower(dir)?;
    let (log, _) =
        SegmentLog::open_read_only(&dir.join("segments"), crate::store::SEGMENT_MAX, dir, None)?;
    check_against_log(&log, mark)?;
    write_mark(dir, mark)
}

/// The refusal of a mark that ends past the log's end `log`.
pub(crate) fn past_end(mark: &FollowerMark, log: Location) -> StoreError {
    refused(Why::PastEnd {
        mark: mark.chain.end,
        log,
    })
}

/// One frame more of the saved-boundary check, of `n` so far: over the bound is its `E_LIMIT`.
fn count_frame(n: u64) -> Result<u64, StoreError> {
    let n = n + 1;
    if n > MAX_CHECK_FRAMES {
        return Err(StoreError::FollowerMarkCheckLimit);
    }
    Ok(n)
}

/// Test hook: the saved-boundary check's bound on `n` frames.
#[doc(hidden)]
pub fn check_frame_count_for_tests(n: u64) -> Result<(), StoreError> {
    count_frame(n - 1).map(drop)
}

/// Test hook: the follow path's replacement of a store's mark, with no check.
#[doc(hidden)]
pub fn replace_mark_for_tests(dir: &Path, mark: &FollowerMark) -> Result<(), StoreError> {
    write_mark(dir, mark)
}

/// Checks `mark` against `log`: the log's repo id, a saved end within the log's, the batch marker
/// that ends there, and the manifest in force there. Damage in the log stays the walk's own
/// error. A suffix past the saved end passes, whatever it holds.
pub(crate) fn check_against_log(log: &SegmentLog, mark: &FollowerMark) -> Result<(), StoreError> {
    let saved = mark.chain.end;
    let mut frames = 0u64;
    let mut repo = None;
    let mut manifest: Option<Vec<u8>> = None;
    let mut marker: Option<BatchMarker> = None;
    log.for_each_view::<StoreError>(
        Location {
            segment: 0,
            offset: 0,
        },
        |_, v, next| {
            frames = count_frame(frames)?;
            match (v.kind, v.payload) {
                (kinds::BATCH_COMMIT, Some(p)) if next == saved => {
                    marker = postcard::from_bytes::<BatchCommit>(p)
                        .ok()
                        .map(|c| BatchMarker::of_commit(&c));
                }
                (kinds::REF_UPDATE, Some(p)) => {
                    if let Ok(u) = postcard::from_bytes::<RefUpdate>(p) {
                        if u.name == REF_REPO_ID {
                            repo = decode_repo_id(&u.value);
                        } else if u.name == format::REF_STORE_FEATURES && next <= saved {
                            manifest = Some(u.value);
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        },
    )?;
    let Some(repo) = repo else {
        return Err(StoreError::Decode("the log holds no meta/repo_id".into()));
    };
    if repo != mark.chain.repo_id {
        return Err(refused(Why::Repo {
            mark: mark.chain.repo_id,
            log: repo,
        }));
    }
    if saved > log.end() {
        return Err(past_end(mark, log.end()));
    }
    if marker != Some(mark.chain.end_batch) {
        return Err(refused(Why::Marker));
    }
    let same = match &mark.chain.store_format {
        DumpStore::Features(m) => manifest.is_some_and(|v| v == m.to_json()),
        DumpStore::Legacy(_) => false,
    };
    if !same {
        return Err(refused(Why::StoreFormat));
    }
    Ok(())
}

/// Why a follow is refused, past the store's and the scanner's own errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowerFailure {
    /// The input is a full dump.
    FullDump,
    /// The input does not continue the saved chain, or the capability is another follower's.
    SavedChain,
    /// The log past the mark is not what the input holds there.
    WrittenPrefix,
    /// A listed project file differs from its saved hash.
    LocalFiles,
    /// The input changed between the check and the append.
    ChangedInput,
    /// The store has no follower mark.
    NotFollower,
}

/// A work bound of one follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowLimit {
    Batches,
    Frames,
    Segments,
    PrefixFrames,
}

/// The most of `unit` one follow takes (inclusive).
pub fn follow_limit(unit: FollowLimit) -> u64 {
    match unit {
        FollowLimit::Batches => 1_000_000,
        FollowLimit::Frames => 10_000_000,
        FollowLimit::Segments => 65_536,
        FollowLimit::PrefixFrames => MAX_CHECK_FRAMES,
    }
}

/// A refused or failed follow.
#[derive(Debug)]
pub enum FollowError {
    Store(StoreError),
    Scan(ScanError),
    Follower(FollowerFailure),
    Limit { unit: FollowLimit },
}

impl From<StoreError> for FollowError {
    fn from(e: StoreError) -> Self {
        FollowError::Store(e)
    }
}

impl From<ScanError> for FollowError {
    fn from(e: ScanError) -> Self {
        FollowError::Scan(e)
    }
}

/// `n` of `unit` so far: over the bound is its refusal.
pub(crate) fn check_follow_limit(unit: FollowLimit, n: u64) -> Result<(), FollowError> {
    if n > follow_limit(unit) {
        return Err(FollowError::Limit { unit });
    }
    Ok(())
}

/// Test hook: the bound check of `n` of `unit`.
#[doc(hidden)]
pub fn check_follow_limit_for_tests(unit: FollowLimit, n: u64) -> Result<(), FollowError> {
    check_follow_limit(unit, n)
}

/// A step of a follow, for a test's kill.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowStep {
    /// The marker of the n-th appended batch of this follow is written.
    Marker(u64),
    /// The appended batches are durable.
    Synced,
    /// The derived index holds the new end.
    Indexed,
    /// One project file was replaced or removed.
    FileReplaced,
    /// The project files and their directories are durable.
    FilesSynced,
}

/// A test observer of a follow's steps.
#[doc(hidden)]
pub type FollowProbe = Box<dyn FnMut(FollowStep)>;

thread_local! {
    static FOLLOW_PROBE: RefCell<Option<FollowProbe>> = const { RefCell::new(None) };
}

/// Test hook: a follow on this thread calls `probe` at each step. None removes it.
#[doc(hidden)]
pub fn on_follow_step(probe: Option<FollowProbe>) {
    FOLLOW_PROBE.with(|p| *p.borrow_mut() = probe);
}

/// Reaches `step` of a follow.
#[doc(hidden)]
pub fn follow_step(step: FollowStep) {
    FOLLOW_PROBE.with(|p| {
        if let Some(f) = p.borrow_mut().as_mut() {
            f(step);
        }
    });
}

/// One increment that [`prepare_follow`] checked against one follower's prefix. Its fields are
/// private and nothing else makes one: it is the only way to [`Store::follow_verified`].
pub struct VerifiedIncrement {
    path: PathBuf,
    id: fileid::FileId,
    env: Envelope,
    plan: ScanPlan,
    /// The saved chain and the actual log end this check bound.
    marked: ChainEnd,
    actual: Location,
    next: ChainEnd,
}

impl VerifiedIncrement {
    /// The chain end the follower holds once this increment is finished.
    pub fn chain(&self) -> &ChainEnd {
        &self.next
    }

    /// The log's actual end when this was checked.
    pub fn actual(&self) -> Location {
        self.actual
    }

    /// The `FILE` records of the increment, as the scanner read them.
    pub fn files(&self) -> &[ProjectFile] {
        &self.plan.files()[0].files
    }

    /// The store mode the prefix set.
    pub fn mode(&self) -> StoreMode {
        self.plan.mode()
    }

    /// Whether the mark already is this increment's end and digest (an idempotent replay).
    pub fn is_marked(&self) -> bool {
        self.next.end == self.marked.end && self.next.digest == self.marked.digest
    }

    pub(crate) fn binds(&self, mark: &FollowerMark, end: Location) -> bool {
        self.marked == mark.chain && self.actual == end
    }

    pub(crate) fn repo(&self) -> rdy_model::RepoId {
        self.next.repo_id
    }

    pub(crate) fn store(&self) -> &DumpStore {
        self.plan.store()
    }
}

/// Checks the increment at `path` against the follower `store` and its saved `mark` (the module
/// docs). Nothing is written. The check order: not a follower, the kind, the saved link, the
/// bounds, the framing and proofs, the bytes already written.
pub fn prepare_follow(
    store: &Store,
    mark: &FollowerMark,
    path: &Path,
) -> Result<VerifiedIncrement, FollowError> {
    store.follow_ready()?;
    let input_error = |source: StoreError| ScanError::Input { file: 0, source };
    let file = std::fs::File::open(path).map_err(|e| input_error(StoreError::Io(e)))?;
    let id = fileid::FileId::of_file(&file).map_err(|e| input_error(StoreError::Io(e)))?;
    let (format, header, trailer, len) = dump::read_envelope(&mut &file).map_err(input_error)?;
    let env = Envelope {
        format,
        header,
        trailer,
        len,
    };
    let chain = env.header.chain;
    if chain.is_some_and(|c| c.kind == DumpKind::Full) {
        return Err(FollowError::Follower(FollowerFailure::FullDump));
    }
    let saved = &mark.chain;
    let log = store.segment_log();
    let actual = log.end();
    let marked = env.header.log_end == saved.end && env.trailer.digest == saved.digest;
    if !marked {
        if let Some(link) = dump_scan::check_saved_link(&env, saved) {
            return Err(FollowError::Scan(ScanError::Link { file: 0, link }));
        }
        if env.header.log_end <= saved.end {
            return Err(FollowError::Follower(FollowerFailure::SavedChain));
        }
        if env.header.log_end < actual {
            return Err(FollowError::Follower(FollowerFailure::WrittenPrefix));
        }
    } else if actual > saved.end {
        return Err(FollowError::Follower(FollowerFailure::SavedChain));
    }
    let Some(chain) = chain else {
        return Err(FollowError::Follower(FollowerFailure::SavedChain));
    };
    check_follow_limit(FollowLimit::Batches, env.trailer.batches)?;
    check_follow_limit(FollowLimit::Frames, env.trailer.frames)?;
    check_follow_limit(
        FollowLimit::Segments,
        u64::from(env.header.log_end.segment) + 1,
    )?;
    if let DumpStore::Features(m) = &env.header.store {
        format::check_writable(m)?;
    }
    let upto = if marked { chain.base } else { saved.end };
    let holes = &log.authorization().holes;
    let segments = store.dir().join("segments");
    let (plan, last) = dump_scan::scan_follow(&segments, holes, upto, path, env.clone())?;
    if marked && last.unwrap_or(BatchMarker::ZERO) != chain.base_batch {
        return Err(FollowError::Follower(FollowerFailure::SavedChain));
    }
    check_whole(log, &plan, path, &env, upto, actual)?;
    let next = ChainEnd {
        repo_id: chain.repo_id,
        store_format: env.header.store.clone(),
        end: env.header.log_end,
        end_batch: chain.end_batch,
        digest: env.trailer.digest,
    };
    Ok(VerifiedIncrement {
        path: path.to_path_buf(),
        id,
        env,
        plan,
        marked: saved.clone(),
        actual,
        next,
    })
}

/// Reads the increment whole with the dump reader, so every payload's hash holds before the first
/// append, and compares each batch the log already holds with the log: its headers, its marker,
/// and each payload's bytes, or the zeros the plan authorizes. The log's end must be a batch end
/// of the increment.
fn check_whole(
    log: &SegmentLog,
    plan: &ScanPlan,
    path: &Path,
    env: &Envelope,
    upto: Location,
    actual: Location,
) -> Result<(), FollowError> {
    let input_error = |source: StoreError| ScanError::Input { file: 0, source };
    let file = std::fs::File::open(path).map_err(|e| input_error(StoreError::Io(e)))?;
    let (mut reader, _, header) = DumpReader::open(BufReader::new(file)).map_err(input_error)?;
    if header != env.header {
        return Err(FollowError::Follower(FollowerFailure::ChangedInput));
    }
    let mut reached = actual == upto;
    while let Some(item) = reader.next().map_err(input_error)? {
        let DumpItem::Batch(b) = item else { continue };
        if b.start >= actual {
            continue;
        }
        let end = compare_batch(log, plan, &b)?;
        if end > actual {
            return Err(FollowError::Follower(FollowerFailure::WrittenPrefix));
        }
        reached |= end == actual;
    }
    if !reached {
        return Err(FollowError::Follower(FollowerFailure::WrittenPrefix));
    }
    Ok(())
}

/// Compares the increment's batch `b` with the log at its place, and returns its end.
fn compare_batch(
    log: &SegmentLog,
    plan: &ScanPlan,
    b: &dump::DumpBatch,
) -> Result<Location, FollowError> {
    let differs = || FollowError::Follower(FollowerFailure::WrittenPrefix);
    let Some(f) = log.readers().get(b.start.segment as usize) else {
        return Err(differs());
    };
    let file_len = f.metadata().map_err(StoreError::Io)?.len();
    let mut place = b.start.offset;
    let mut payload: Vec<u8> = Vec::new();
    let mut hashes: Vec<rdy_model::Hash> = Vec::new();
    for frame in &b.frames {
        let len = frame.original_len();
        let head = crate::segment::header_at(f, place, file_len).map_err(StoreError::Io)?;
        if head != Some((len, frame.kind(), *frame.hash())) {
            return Err(differs());
        }
        hashes.push(*frame.hash());
        let start = place + FRAME_HEADER;
        let zero =
            crate::segment::payload_is_zero(f, start, u64::from(len)).map_err(StoreError::Io)?;
        match frame {
            Frame::Dropped { .. } => {
                if !zero {
                    return Err(differs());
                }
            }
            Frame::Present { payload: want, .. } => {
                let at = Location {
                    segment: b.start.segment,
                    offset: place,
                };
                let zeroed = matches!(
                    plan.frame_action(at, frame.kind(), *frame.hash(), len)?,
                    FrameAction::Zero { .. }
                );
                if !(zero && zeroed) {
                    payload.clear();
                    payload
                        .try_reserve_exact(len as usize)
                        .map_err(|_| dump::damaged(0, dump::drop_text::ALLOCATION))?;
                    payload.resize(len as usize, 0);
                    pio::read_exact_at(f, &mut payload, start).map_err(StoreError::Io)?;
                    if payload != *want {
                        return Err(differs());
                    }
                }
            }
        }
        place = start + u64::from(len);
    }
    let digest = dump::try_batch_digest(hashes.iter()).ok_or_else(|| dump::damaged(0, "digest"))?;
    let marker = crate::store::encode(&BatchCommit {
        start: b.start,
        frames: b.frames.len() as u32,
        digest,
    });
    let head = crate::segment::header_at(f, place, file_len).map_err(StoreError::Io)?;
    if head
        != Some((
            marker.len() as u32,
            kinds::BATCH_COMMIT,
            rdy_model::Hash::of(&marker),
        ))
    {
        return Err(differs());
    }
    Ok(Location {
        segment: b.start.segment,
        offset: place + FRAME_HEADER + marker.len() as u64,
    })
}

/// Appends the batches of `v` the log does not hold yet, in one ingest, with no sync. On an error
/// it also returns where the last complete batch ends: the unsealed tail past it is the caller's
/// to cut. The input's identity, length and envelope must still be those it checked.
pub(crate) fn ingest_verified(
    log: &mut SegmentLog,
    dir: &Path,
    v: &VerifiedIncrement,
    mode: StoreMode,
    rules: RefRules,
) -> Result<(u64, u64), (FollowError, Location)> {
    let mut sealed = log.end();
    let changed = || FollowError::Follower(FollowerFailure::ChangedInput);
    let again = (|| {
        let file = std::fs::File::open(&v.path).map_err(|_| changed())?;
        let same = fileid::FileId::of_file(&file).is_ok_and(|id| id == v.id)
            && file.metadata().is_ok_and(|m| m.len() == v.env.len);
        if !same {
            return Err(changed());
        }
        let (reader, _, header) = DumpReader::open(BufReader::new(file)).map_err(|_| changed())?;
        if header != v.env.header {
            return Err(changed());
        }
        Ok(reader)
    })();
    let mut reader = match again {
        Ok(r) => r,
        Err(e) => return Err((e, sealed)),
    };
    let actual = v.actual;
    let mut ingest = Ingest::appending(log, dir, mode, rules);
    let mut n = 0u64;
    loop {
        // a difference from the first read is the input's change, not its damage
        let item = match reader.next() {
            Ok(i) => i,
            Err(StoreError::Io(e)) => return Err((FollowError::Store(StoreError::Io(e)), sealed)),
            Err(_) => return Err((changed(), sealed)),
        };
        let Some(item) = item else { break };
        let DumpItem::Batch(b) = item else { continue };
        if b.start < actual {
            continue;
        }
        n += 1;
        if let Err(e) = ingest.batch_with_plan(n, &b, &v.plan) {
            return Err((FollowError::Store(e), sealed));
        }
        sealed = ingest.end();
        follow_step(FollowStep::Marker(n));
    }
    if reader.trailer() != Some(&v.env.trailer) {
        return Err((changed(), sealed));
    }
    if let DumpStore::Features(m) = v.plan.store()
        && let Err(e) = ingest.finish_append(m)
    {
        return Err((FollowError::Store(e), sealed));
    }
    Ok(ingest.appended())
}

/// What a follow's ingest reports, by `end`.
pub(crate) fn report(appended: (u64, u64), end: Location) -> IngestReport {
    IngestReport {
        batches: appended.0,
        frames: appended.1,
        end,
    }
}
