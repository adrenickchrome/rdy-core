use crate::fsutil::sync_dir;
use crate::kinds::{self, BatchCommit};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub(crate) const HEADER: u64 = 4 + 1 + 32;
/// An upper bound on the encoded `BatchCommit` payload, used to size a batch before it is written.
const MARKER_MAX: u64 = 64;

/// Where a test child process dies (`std::process::abort`) during a write or a checkpoint.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    BeforeMarker,
    RolledBeforeMarker,
    BeforeSync,
    AfterSync,
    AfterIndex,
    BeforeCheckpoint,
    AfterCheckpoint,
    /// The close of a store whose index is stale (after a failed seal), right after the store
    /// drops its handle on the database.
    StaleIndexClosed,
}

impl CrashPoint {
    pub const ALL: [CrashPoint; 8] = [
        CrashPoint::BeforeMarker,
        CrashPoint::RolledBeforeMarker,
        CrashPoint::BeforeSync,
        CrashPoint::AfterSync,
        CrashPoint::AfterIndex,
        CrashPoint::BeforeCheckpoint,
        CrashPoint::AfterCheckpoint,
        CrashPoint::StaleIndexClosed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CrashPoint::BeforeMarker => "before_marker",
            CrashPoint::RolledBeforeMarker => "rolled_before_marker",
            CrashPoint::BeforeSync => "before_sync",
            CrashPoint::AfterSync => "after_sync",
            CrashPoint::AfterIndex => "after_index",
            CrashPoint::BeforeCheckpoint => "before_checkpoint",
            CrashPoint::AfterCheckpoint => "after_checkpoint",
            CrashPoint::StaleIndexClosed => "stale_index_closed",
        }
    }

    pub fn parse(s: &str) -> Option<CrashPoint> {
        CrashPoint::ALL.into_iter().find(|p| p.name() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Location {
    pub segment: u32,
    pub offset: u64,
}

pub struct SegmentLog {
    dir: PathBuf,
    readers: Vec<File>,
    writer: File,
    len: u64,
    max_len: u64,
    /// Test fault injection (see `fail_next_roll` / `fail_next_append_after`).
    fail_roll: bool,
    fail_partial: Option<usize>,
    /// Test hook: the armed crash point (see `crash_at`).
    crash: Option<CrashPoint>,
}

/// The path of segment `id` in `dir`.
pub fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("seg-{id:06}.log"))
}

/// If the segment files in `dir` skip a number: the first missing number and the next number
/// present after it. Only `seg-<digits>.log` names count; quarantined `.torn-` copies don't.
pub fn find_gap(dir: &Path) -> io::Result<Option<(u32, u32)>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut ids: Vec<u32> = vec![];
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(n) = name
            .to_str()
            .and_then(|n| n.strip_prefix("seg-"))
            .and_then(|n| n.strip_suffix(".log"))
        else {
            continue;
        };
        if n.len() >= 6
            && n.bytes().all(|b| b.is_ascii_digit())
            && let Ok(id) = n.parse()
        {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    for (want, have) in (0u32..).zip(&ids) {
        if *have != want {
            return Ok(Some((want, *have)));
        }
    }
    Ok(None)
}

thread_local! {
    /// Payload bytes this thread's frame reads have hashed (see [`frame_bytes_hashed`]).
    static HASHED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: the payload bytes frame reads on this thread have hashed so far, so a test can
/// measure what an open's tail check costs.
#[doc(hidden)]
pub fn frame_bytes_hashed() -> u64 {
    HASHED.with(|h| h.get())
}

fn count_hashed(n: usize) {
    HASHED.with(|h| h.set(h.get() + n as u64));
}

/// Reads the frame at `offset`, or returns None if it is torn or corrupt.
fn read_frame(f: &File, offset: u64, file_len: u64) -> io::Result<Option<(u8, Hash, Vec<u8>)>> {
    let Some(payload_start) = offset.checked_add(HEADER).filter(|end| *end <= file_len) else {
        return Ok(None);
    };
    let mut header = [0u8; HEADER as usize];
    f.read_exact_at(&mut header, offset)?;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as u64;
    if payload_start
        .checked_add(len)
        .is_none_or(|end| end > file_len)
    {
        return Ok(None);
    }
    let kind = header[4];
    let hash = Hash(header[5..37].try_into().unwrap());
    let mut payload = vec![0u8; len as usize];
    f.read_exact_at(&mut payload, payload_start)?;
    count_hashed(payload.len());
    if Hash::of(&payload) != hash {
        return Ok(None);
    }
    Ok(Some((kind, hash, payload)))
}

/// A sealed segment (one the log has rolled past) that does not end at a batch marker: the log
/// rolls only between batches, so frames at its end are missing.
fn unsealed(seg: u32) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "segment {seg} does not end at a batch marker, but a later segment was started: frames are missing; restore it from backup"
        ),
    )
}

/// True if `f` (`file_len` bytes) ends with a hash-valid `BATCH_COMMIT` frame, the shape every
/// sealed segment has. One read of the file's last `HEADER + MARKER_MAX` bytes: a candidate
/// frame start is any position whose frame would end exactly at the end of the file.
fn ends_at_marker(f: &File, file_len: u64) -> io::Result<bool> {
    let tail = file_len.min(HEADER + MARKER_MAX);
    let mut buf = vec![0u8; tail as usize];
    f.read_exact_at(&mut buf, file_len - tail)?;
    let header = HEADER as usize;
    for pos in 0..buf.len().saturating_sub(header - 1) {
        let rest = &buf[pos..];
        let len = u32::from_le_bytes(rest[0..4].try_into().unwrap()) as usize;
        if rest[4] != kinds::BATCH_COMMIT || header + len != rest.len() {
            continue;
        }
        if Hash::of(&rest[header..]).0[..] == rest[5..header] {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The first sealed segment in `dir` (one a later segment follows) that does not end at a batch
/// marker. The log rolls only between batches, so such a segment has lost frames at its end.
/// One small read per sealed segment; call it after `find_gap`.
pub fn find_unsealed(dir: &Path) -> io::Result<Option<u32>> {
    let mut seg = 0;
    while seg_path(dir, seg + 1).exists() {
        let f = File::open(seg_path(dir, seg))?;
        if !ends_at_marker(&f, f.metadata()?.len())? {
            return Ok(Some(seg));
        }
        seg += 1;
    }
    Ok(None)
}

/// The read half of the log (rdr1): positioned reads of frames at locations the caller already
/// knows are acknowledged (from the index, or bounded by an explicit `upto`). It opens each
/// segment file read-only on first use and keeps it; it never scans for the end of a segment, so
/// it never meets a batch the writer is still appending.
pub struct SegmentReader {
    dir: PathBuf,
    files: RwLock<BTreeMap<u32, Arc<File>>>,
}

/// A frame that ends past the end of its file, or whose payload does not match its hash.
fn corrupt_frame(loc: Location) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("corrupt or missing frame at {loc:?}"),
    )
}

impl SegmentReader {
    /// A reader of the segments in `dir`. It opens nothing yet.
    pub fn open(dir: &Path) -> SegmentReader {
        SegmentReader {
            dir: dir.to_path_buf(),
            files: RwLock::new(BTreeMap::new()),
        }
    }

    /// Segment `seg`'s file, opened read-only on first use. A missing file is `NotFound`.
    fn file(&self, seg: u32) -> io::Result<Arc<File>> {
        if let Some(f) = self
            .files
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&seg)
        {
            return Ok(Arc::clone(f));
        }
        let f = Arc::new(File::open(seg_path(&self.dir, seg))?);
        let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
        Ok(Arc::clone(files.entry(seg).or_insert(f)))
    }

    /// The frame at `loc` and the payload hash its header carries, hash-checked as
    /// `SegmentLog::read_hashed` checks it: a frame that does not fit its file or does not match
    /// its hash is `InvalidData`.
    pub fn read_hashed(&self, loc: Location) -> io::Result<(u8, Hash, Vec<u8>)> {
        let f = self.file(loc.segment)?;
        let (kind, hash, payload, _) = read_frame_at(&f, loc)?;
        Ok((kind, hash, payload))
    }

    pub fn read(&self, loc: Location) -> io::Result<(u8, Vec<u8>)> {
        let (kind, _, payload) = self.read_hashed(loc)?;
        Ok((kind, payload))
    }

    /// Calls `f(location, kind, hash, payload, next)` for every frame in `[from, upto)`, in order;
    /// `next` is the location just past the frame. A segment before `upto`'s is read to the end of
    /// its file (the writer has rolled past it, so it ends at a marker), then the walk continues
    /// with the next segment. A frame that crosses `upto`, a `from` past the end of its segment,
    /// or a segment before `upto`'s that does not end at a marker, is `InvalidData`. An error
    /// from `f` stops the walk and is returned.
    pub fn for_each_from<E: From<io::Error>>(
        &self,
        from: Location,
        upto: Location,
        mut f: impl FnMut(Location, u8, Hash, &[u8], Location) -> Result<(), E>,
    ) -> Result<(), E> {
        if from >= upto {
            return Ok(());
        }
        for seg in from.segment..=upto.segment {
            if seg == upto.segment && upto.offset == 0 {
                // nothing of `upto`'s segment is in range
                break;
            }
            let file = self.file(seg)?;
            let end = if seg == upto.segment {
                upto.offset
            } else {
                file.metadata()?.len()
            };
            let mut offset = if seg == from.segment { from.offset } else { 0 };
            if offset > end {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt frame in segment {seg} at offset {offset}"),
                )
                .into());
            }
            let sealed = seg < upto.segment;
            if sealed && offset == 0 && end == 0 {
                return Err(unsealed(seg).into());
            }
            while offset < end {
                let loc = Location {
                    segment: seg,
                    offset,
                };
                let (kind, hash, payload, next) = read_frame_at(&file, loc)?;
                if next > end {
                    return Err(corrupt_frame(loc).into());
                }
                let next = Location {
                    segment: seg,
                    offset: next,
                };
                if sealed && next.offset == end && kind != kinds::BATCH_COMMIT {
                    return Err(unsealed(seg).into());
                }
                f(loc, kind, hash, &payload, next)?;
                offset = next.offset;
            }
        }
        Ok(())
    }
}

/// The frame at `loc` in `f`, by positioned reads of its header and payload, with the offset just
/// past it. A frame that does not fit the file or does not match its hash is `InvalidData`, as
/// `SegmentLog::read_hashed` has it.
fn read_frame_at(f: &File, loc: Location) -> io::Result<(u8, Hash, Vec<u8>, u64)> {
    let file_len = f.metadata()?.len();
    let (kind, hash, payload) =
        read_frame(f, loc.offset, file_len)?.ok_or_else(|| corrupt_frame(loc))?;
    let next = loc.offset + HEADER + payload.len() as u64;
    Ok((kind, hash, payload, next))
}

/// True if any valid frame (header parses, fits within EOF, payload hash verifies)
/// starts after `offset`. A genuine torn tail never has one.
fn valid_frame_after(f: &File, offset: u64, file_len: u64) -> io::Result<bool> {
    let mut buf = vec![0u8; (file_len - offset) as usize];
    f.read_exact_at(&mut buf, offset)?;
    let h = HEADER as usize;
    for pos in 1..buf.len() {
        if pos + h > buf.len() {
            break;
        }
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let Some(end) = (pos + h).checked_add(len) else {
            continue;
        };
        if end > buf.len() {
            continue;
        }
        if Hash::of(&buf[pos + h..end]).0 == buf[pos + 5..pos + 37] {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copies `[offset, file_len)` to `<seg>.torn-<offset>` (or `.torn-<offset>.<n>` if that name is
/// taken; an existing quarantine file is never overwritten) and fsyncs it and the directory.
/// Returns the path it wrote.
fn quarantine(f: &File, dir: &Path, seg: &Path, offset: u64, file_len: u64) -> io::Result<PathBuf> {
    let mut buf = vec![0u8; (file_len - offset) as usize];
    f.read_exact_at(&mut buf, offset)?;
    let base = format!(
        "{}.torn-{offset}",
        seg.file_name().unwrap().to_string_lossy()
    );
    let mut n = 0u32;
    let (q, path) = loop {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}.{n}")
        };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(q) => break (q, path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    };
    q.write_all_at(&buf, 0)?;
    q.sync_all()?;
    sync_dir(dir)?;
    Ok(path)
}

/// True if the batch marker `payload`, met after the data frames `since` (offset and hash of each
/// frame since the previous valid marker) in segment `seg`, matches its batch: it names this
/// segment, its `start` is one of those frames, and the frame count and digest from there agree.
/// Frames before the marker's `start` are allowed: a marker vouches only for its own batch. A
/// store write never leaves such frames (a failed write is truncated away or poisons the store);
/// `append` does.
fn marker_matches(seg: u32, since: &[(u64, Hash)], payload: &[u8]) -> bool {
    let Ok(m) = postcard::from_bytes::<BatchCommit>(payload) else {
        return false;
    };
    if m.start.segment != seg {
        return false;
    }
    let Some(i) = since.iter().position(|(o, _)| *o == m.start.offset) else {
        return false;
    };
    since.len() - i == m.frames as usize
        && kinds::batch_digest(since[i..].iter().map(|(_, h)| h)) == m.digest
}

/// The result of scanning one segment forward from offset 0 under the marker rule.
struct Scan {
    /// The end of the last marker that matches its batch; None if there is none.
    boundary: Option<u64>,
    /// Where the scan stopped: a torn or corrupt frame, a marker that does not match its batch,
    /// or the end of the file.
    stop: u64,
}

fn scan_marked(f: &File, seg: u32, file_len: u64) -> io::Result<Scan> {
    let mut offset = 0;
    let mut boundary = None;
    let mut since: Vec<(u64, Hash)> = vec![];
    while let Some((kind, hash, payload)) = read_frame(f, offset, file_len)? {
        if kind == kinds::BATCH_COMMIT {
            if !marker_matches(seg, &since, &payload) {
                break;
            }
            since.clear();
            offset += HEADER + payload.len() as u64;
            boundary = Some(offset);
        } else {
            since.push((offset, hash));
            offset += HEADER + payload.len() as u64;
        }
    }
    Ok(Scan {
        boundary,
        stop: offset,
    })
}

/// What one `acknowledged_batch_follows` call has learned about this file, so hostile bytes in a
/// torn tail cost each frame at most one read and hash, however many marker-shaped records name
/// it.
#[derive(Default)]
struct TailMemo {
    /// Frames read and hashed so far: offset -> (kind, hash, end) of a valid frame, or None for
    /// one whose payload does not match its hash.
    frames: BTreeMap<u64, Option<(u8, Hash, u64)>>,
    /// Batch starts whose chain of frames meets a frame that is invalid on its own (a hash
    /// mismatch, or a marker inside the batch). Candidates are tried in file order, so every
    /// later marker naming such a start meets the same frame before it reaches itself.
    dead_starts: BTreeSet<u64>,
}

/// One step of a candidate batch's chain: the frame at `offset` if it ends at or before `bound`.
enum Step {
    /// A valid frame: kind, hash and end.
    Frame(u8, Hash, u64),
    /// The frame does not fit before `bound` (or the file): this candidate fails, but a later
    /// one with a larger bound may not.
    PastBound,
    /// The frame's payload does not match its hash: every candidate that reaches it fails.
    Invalid,
}

/// Reads the frame at `offset` for the tail check. Its header is read first, and its payload is
/// read and hashed only if the frame ends at or before `bound`, at most once per `memo`.
fn tail_frame(f: &File, offset: u64, bound: u64, memo: &mut TailMemo) -> io::Result<Step> {
    if let Some(known) = memo.frames.get(&offset) {
        return Ok(match *known {
            Some((kind, hash, end)) if end <= bound => Step::Frame(kind, hash, end),
            Some(_) => Step::PastBound,
            None => Step::Invalid,
        });
    }
    let Some(payload_start) = offset.checked_add(HEADER).filter(|end| *end <= bound) else {
        return Ok(Step::PastBound);
    };
    let mut header = [0u8; HEADER as usize];
    f.read_exact_at(&mut header, offset)?;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as u64;
    let Some(end) = payload_start.checked_add(len).filter(|end| *end <= bound) else {
        return Ok(Step::PastBound);
    };
    let hash = Hash(header[5..37].try_into().unwrap());
    let mut payload = vec![0u8; len as usize];
    f.read_exact_at(&mut payload, payload_start)?;
    count_hashed(payload.len());
    let found = (Hash::of(&payload) == hash).then_some((header[4], hash, end));
    memo.frames.insert(offset, found);
    Ok(match found {
        Some((kind, hash, end)) => Step::Frame(kind, hash, end),
        None => Step::Invalid,
    })
}

/// True if the batch marker at absolute offset `marker_at` (payload `payload`) closes a real batch
/// of this file: its `start` is before the marker, and the frames from `start` are hash-valid,
/// contiguous, hold no other marker, and end exactly at `marker_at`, with the frame count and
/// digest the marker names. Marker-shaped bytes inside another frame's payload (a copy of a
/// segment, crafted asset bytes) carry offsets of a different file and fail this. No frame of the
/// chain is read past `marker_at`.
fn closes_a_real_batch(
    f: &File,
    seg: u32,
    marker_at: u64,
    payload: &[u8],
    memo: &mut TailMemo,
) -> io::Result<bool> {
    let Ok(m) = postcard::from_bytes::<BatchCommit>(payload) else {
        return Ok(false);
    };
    if m.start.segment != seg
        || m.start.offset >= marker_at
        || m.frames == 0
        || memo.dead_starts.contains(&m.start.offset)
    {
        return Ok(false);
    }
    let mut since: Vec<(u64, Hash)> = vec![];
    let mut offset = m.start.offset;
    while offset < marker_at {
        if since.len() >= m.frames as usize {
            return Ok(false);
        }
        match tail_frame(f, offset, marker_at, memo)? {
            Step::Frame(kind, hash, end) if kind != kinds::BATCH_COMMIT => {
                since.push((offset, hash));
                offset = end;
            }
            Step::PastBound => return Ok(false),
            Step::Frame(..) | Step::Invalid => {
                memo.dead_starts.insert(m.start.offset);
                return Ok(false);
            }
        }
    }
    Ok(offset == marker_at && marker_matches(seg, &since, payload))
}

/// True if a real batch, closed by a hash-valid `BATCH_COMMIT` frame at any byte position at or
/// after `stop`, starts past `boundary`: an acknowledged batch follows the damage. Only positions
/// whose kind byte is `BATCH_COMMIT` and whose length field fits a marker are hashed, and a
/// candidate counts only if its batch chains from its `start` to the marker
/// (`closes_a_real_batch`). Candidates share one memo, so each frame is hashed at most once.
fn acknowledged_batch_follows(
    f: &File,
    seg: u32,
    boundary: u64,
    stop: u64,
    file_len: u64,
) -> io::Result<bool> {
    let mut buf = vec![0u8; (file_len - stop) as usize];
    f.read_exact_at(&mut buf, stop)?;
    let h = HEADER as usize;
    let mut memo = TailMemo::default();
    for pos in 0..buf.len() {
        if pos + h > buf.len() {
            break;
        }
        if buf[pos + 4] != kinds::BATCH_COMMIT {
            continue;
        }
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as u64;
        if len > MARKER_MAX || pos + h + len as usize > buf.len() {
            continue;
        }
        let payload = &buf[pos + h..pos + h + len as usize];
        if Hash::of(payload).0 != buf[pos + 5..pos + 37] {
            continue;
        }
        if let Ok(m) = postcard::from_bytes::<BatchCommit>(payload)
            && m.start.offset > boundary
            && closes_a_real_batch(f, seg, stop + pos as u64, payload, &mut memo)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn damaged_error(stop: u64, seg: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "corruption at offset {stop} in {} damages acknowledged data; refusing to truncate. Frames before offset {stop} are intact: move the damaged segment aside or restore it from backup, then reopen",
            seg.display()
        ),
    )
}

/// The marker rule's tail check on segment `seg` (file `f`, `file_len` bytes) once its scan has
/// found `boundary` `b` and stopped at `stop`. With no tail (`b` is the end of the file) it does
/// nothing. Otherwise it refuses, leaving the file untouched, if an acknowledged batch follows
/// the damage or the index's `floor` is past `(seg, b)`; else it quarantines `[b, file_len)` and
/// returns the path. It never truncates: the caller does.
fn check_tail(
    f: &File,
    dir: &Path,
    seg: u32,
    scan: &Scan,
    file_len: u64,
    floor: Option<Location>,
) -> io::Result<Option<PathBuf>> {
    let b = scan.boundary.unwrap_or(0);
    if b >= file_len {
        return Ok(None);
    }
    let path = seg_path(dir, seg);
    let end_of_good = Location {
        segment: seg,
        offset: b,
    };
    if floor.is_some_and(|fl| fl > end_of_good)
        || acknowledged_batch_follows(f, seg, b, scan.stop, file_len)?
    {
        return Err(damaged_error(scan.stop, &path));
    }
    quarantine(f, dir, &path, b, file_len).map(Some)
}

/// Opens every segment file in `dir` for reading, creating `dir` and an empty segment 0 if there
/// are none. Refuses a gap in the segment numbers.
fn open_readers(dir: &Path) -> io::Result<Vec<File>> {
    crate::fsutil::create_dir_durable(dir)?;
    // seg1: a segment past a gap holds acknowledged frames the log would otherwise skip
    if let Some((missing, later)) = find_gap(dir)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is missing but {} exists: acknowledged frames are missing; restore the missing segment from backup, then reopen",
                seg_path(dir, missing).display(),
                seg_path(dir, later).display()
            ),
        ));
    }
    let mut readers = vec![];
    while seg_path(dir, readers.len() as u32).exists() {
        readers.push(File::open(seg_path(dir, readers.len() as u32))?);
    }
    if readers.is_empty() {
        File::create(seg_path(dir, 0))?.sync_all()?;
        sync_dir(dir)?;
        readers.push(File::open(seg_path(dir, 0))?);
    }
    Ok(readers)
}

/// True if the last two segments hold a `BATCH_COMMIT`: the log was written by M2 and opens under
/// the marker rule. Each segment is read in order until its first bad frame. Behind that damage a
/// marker still counts if it closes a real batch of the file (see `acknowledged_batch_follows`),
/// so damage in the first batch does not make an M2 log look like an M1 log.
pub fn log_has_marker(dir: &Path) -> io::Result<bool> {
    let mut count = 0u32;
    while seg_path(dir, count).exists() {
        count += 1;
    }
    for seg in count.saturating_sub(2)..count {
        let f = File::open(seg_path(dir, seg))?;
        let file_len = f.metadata()?.len();
        let mut offset = 0;
        while let Some((kind, _, payload)) = read_frame(&f, offset, file_len)? {
            if kind == kinds::BATCH_COMMIT {
                return Ok(true);
            }
            offset += HEADER + payload.len() as u64;
        }
        if offset < file_len && acknowledged_batch_follows(&f, seg, 0, offset, file_len)? {
            return Ok(true);
        }
    }
    Ok(false)
}

impl SegmentLog {
    pub fn open(dir: &Path, max_len: u64) -> io::Result<SegmentLog> {
        SegmentLog::open_acked(dir, max_len, None)
    }

    /// Opens the log, repairing a torn tail. `acked` is the end of the acknowledged log (the
    /// index's `indexed_upto`), or None when unknown. In the last segment, corruption at or
    /// beyond `acked` belongs to an unacknowledged batch: it is quarantined and truncated even if
    /// valid frames follow (one batch is fsynced once, so its frames can tear independently).
    /// Corruption below `acked` damages acknowledged data and is refused. With `acked` unknown,
    /// the tail is truncated only if no valid frame follows the corruption.
    pub fn open_acked(dir: &Path, max_len: u64, acked: Option<Location>) -> io::Result<SegmentLog> {
        let readers = open_readers(dir)?;
        let last = (readers.len() - 1) as u32;
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(seg_path(dir, last))?;
        let file_len = writer.metadata()?.len();
        let mut offset = 0;
        while let Some((_, _, payload)) = read_frame(&writer, offset, file_len)? {
            offset += HEADER + payload.len() as u64;
        }
        if offset < file_len {
            let unacknowledged = match acked {
                Some(a) => a.segment < last || (a.segment == last && offset >= a.offset),
                None => !valid_frame_after(&writer, offset, file_len)?,
            };
            if !unacknowledged {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "corruption at offset {offset} in {} damages acknowledged data; refusing to truncate. Frames before offset {offset} are intact: move the damaged segment aside or restore it from backup, then reopen",
                        seg_path(dir, last).display()
                    ),
                ));
            }
            quarantine(&writer, dir, &seg_path(dir, last), offset, file_len)?;
            writer.set_len(offset)?;
        }
        // a kill can leave complete frames that were written but never fsynced: this open accepts
        // them, so make them durable before an index checkpoint can get ahead of them
        writer.sync_all()?;
        crate::fsutil::record(&seg_path(dir, last));
        let log = SegmentLog {
            dir: dir.to_path_buf(),
            readers,
            writer,
            len: offset,
            max_len,
            fail_roll: false,
            fail_partial: None,
            crash: None,
        };
        Ok(log)
    }

    /// Opens the log under the marker rule: the acknowledged boundary is the end of the last batch
    /// marker of the last segment that matches its batch. A tail after it (a torn or unmarked
    /// batch) is quarantined and cut; damage before it is refused, leaving the files untouched.
    /// `floor` is the index's durable `indexed_upto` (None if unknown): a tail that starts before
    /// it would cut acknowledged frames, so it is refused too. A last segment holding no marker
    /// is one unacknowledged batch: it is quarantined if non-empty and removed (segment 0 is
    /// truncated instead), provided the segment before it ends exactly at a marker. Returns the
    /// log and the quarantine files this open wrote. It ends by fsyncing the last segment, so
    /// frames a kill left unsynced are durable before the index can be checkpointed past them.
    pub fn open_marked(
        dir: &Path,
        max_len: u64,
        floor: Option<Location>,
    ) -> io::Result<(SegmentLog, Vec<PathBuf>)> {
        let mut readers = open_readers(dir)?;
        let mut last = (readers.len() - 1) as u32;
        let mut writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(seg_path(dir, last))?;
        let file_len = writer.metadata()?.len();
        let scan = scan_marked(&writer, last, file_len)?;
        let mut quarantined = vec![];
        let len;
        if scan.boundary.is_none() && last > 0 {
            // the last segment is one unacknowledged batch, if the one before it is sealed at a marker
            let prev_path = seg_path(dir, last - 1);
            let prev = File::open(&prev_path)?;
            let prev_len = prev.metadata()?.len();
            if scan_marked(&prev, last - 1, prev_len)?.boundary != Some(prev_len) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} does not end at a batch marker, but a later segment was started: the log is damaged; restore it from backup, then reopen",
                        prev_path.display()
                    ),
                ));
            }
            quarantined.extend(check_tail(&writer, dir, last, &scan, file_len, floor)?);
            drop(writer);
            std::fs::remove_file(seg_path(dir, last))?;
            sync_dir(dir)?;
            readers.pop();
            last -= 1;
            writer = OpenOptions::new().read(true).write(true).open(&prev_path)?;
            len = prev_len;
        } else {
            let b = scan.boundary.unwrap_or(0);
            if let Some(q) = check_tail(&writer, dir, last, &scan, file_len, floor)? {
                quarantined.push(q);
                writer.set_len(b)?;
            }
            len = b;
        }
        // a kill can leave a complete, marked batch that was written but never fsynced: this
        // open accepts it, so make it durable before an index checkpoint can get ahead of it
        writer.sync_all()?;
        crate::fsutil::record(&seg_path(dir, last));
        let log = SegmentLog {
            dir: dir.to_path_buf(),
            readers,
            writer,
            len,
            max_len,
            fail_roll: false,
            fail_partial: None,
            crash: None,
        };
        Ok((log, quarantined))
    }

    /// Discards everything appended at or after `to`, which must not be before the start of the
    /// segment that was current when the failed batch began. Segments created after `to` are
    /// removed. Only for rolling back frames that were never acknowledged.
    pub fn truncate_to(&mut self, to: Location) -> io::Result<()> {
        if to.segment > self.current() || (to.segment == self.current() && to.offset > self.len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot truncate to {to:?}: past the end {:?}", self.end()),
            ));
        }
        if to.segment < self.current() {
            self.writer = OpenOptions::new()
                .read(true)
                .write(true)
                .open(seg_path(&self.dir, to.segment))?;
            while self.current() > to.segment {
                let id = self.current();
                self.readers.pop();
                std::fs::remove_file(seg_path(&self.dir, id))?;
            }
            sync_dir(&self.dir)?;
        }
        self.writer.set_len(to.offset)?;
        self.writer.sync_all()?;
        self.len = to.offset;
        // a roll that failed after creating its file may have left the next segment behind
        let stray = seg_path(&self.dir, self.current() + 1);
        if stray.exists() {
            std::fs::remove_file(&stray)?;
            sync_dir(&self.dir)?;
        }
        Ok(())
    }

    fn current(&self) -> u32 {
        (self.readers.len() - 1) as u32
    }

    pub fn end(&self) -> Location {
        Location {
            segment: self.current(),
            offset: self.len,
        }
    }

    /// Test hook: the next roll to a new segment fails after creating the new file.
    #[doc(hidden)]
    pub fn fail_next_roll(&mut self) {
        self.fail_roll = true;
    }

    /// Test hook: the next append writes only the first `bytes` bytes of its frame, then fails.
    #[doc(hidden)]
    pub fn fail_next_append_after(&mut self, bytes: usize) {
        self.fail_partial = Some(bytes);
    }

    /// Starts segment `current + 1`. The new writer, reader and length are committed together,
    /// only once the file and the directory are durable; on any failure the log is exactly as
    /// before and the new file is removed best-effort (`truncate_to` sweeps a leftover next segment).
    fn roll(&mut self) -> io::Result<()> {
        self.writer.sync_all()?;
        let path = seg_path(&self.dir, self.readers.len() as u32);
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        match self.finish_roll(&writer, &path) {
            Ok(reader) => {
                self.writer = writer;
                self.readers.push(reader);
                self.len = 0;
                Ok(())
            }
            Err(e) => {
                drop(writer);
                let _ = std::fs::remove_file(&path);
                let _ = sync_dir(&self.dir);
                Err(e)
            }
        }
    }

    fn finish_roll(&mut self, writer: &File, path: &Path) -> io::Result<File> {
        writer.sync_all()?;
        if std::mem::take(&mut self.fail_roll) {
            return Err(io::Error::other("injected roll failure"));
        }
        sync_dir(&self.dir)?;
        File::open(path)
    }

    /// Appends one frame with no batch marker, for tests. Rolls first if it would not fit in the
    /// current segment.
    pub fn append(&mut self, kind: u8, payload: &[u8]) -> io::Result<(Location, Hash)> {
        if payload.len() > u32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }
        let frame_len = HEADER + payload.len() as u64;
        if self.len > 0 && self.len + frame_len > self.max_len {
            self.roll()?;
        }
        self.write_frame(kind, payload)
    }

    /// Writes one frame at the end of the current segment, with no size check and no roll.
    fn write_frame(&mut self, kind: u8, payload: &[u8]) -> io::Result<(Location, Hash)> {
        let frame_len = HEADER + payload.len() as u64;
        let hash = Hash::of(payload);
        let mut buf = Vec::with_capacity(frame_len as usize);
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.push(kind);
        buf.extend_from_slice(&hash.0);
        buf.extend_from_slice(payload);
        if let Some(n) = self.fail_partial.take() {
            self.writer
                .write_all_at(&buf[..n.min(buf.len())], self.len)?;
            return Err(io::Error::other("injected partial append"));
        }
        self.writer.write_all_at(&buf, self.len)?;
        let loc = Location {
            segment: self.current(),
            offset: self.len,
        };
        self.len += frame_len;
        Ok((loc, hash))
    }

    /// Appends `frames` as one batch ending in a `BATCH_COMMIT` marker, and does not sync. The
    /// segment rolls only before the batch, never inside it: a batch larger than a segment gets a
    /// segment of its own. On error the caller rolls back with `truncate_to`.
    pub fn append_batch(&mut self, frames: &[(u8, Vec<u8>)]) -> io::Result<Vec<(Location, Hash)>> {
        if frames.is_empty() {
            return Ok(vec![]);
        }
        let before = self.current();
        let placed = self.append_frames(frames, true)?;
        let start = placed[0].0;
        self.crash_point(if start.segment != before {
            CrashPoint::RolledBeforeMarker
        } else {
            CrashPoint::BeforeMarker
        });
        self.append_marker(start, placed.iter().map(|(_, h)| h))?;
        Ok(placed)
    }

    /// Appends `frames` with no batch marker and does not sync; returns where each went and its
    /// hash. With `may_roll`, the first frames of a batch, the segment rolls first when the frames
    /// and a marker would not fit in it (as `append_batch` does); without it (a later write of a
    /// held batch) it never rolls, so a batch stays in one segment and the limit is soft for it.
    /// On error the caller rolls back with `truncate_to`.
    pub fn append_frames(
        &mut self,
        frames: &[(u8, Vec<u8>)],
        may_roll: bool,
    ) -> io::Result<Vec<(Location, Hash)>> {
        if frames.is_empty() {
            return Ok(vec![]);
        }
        if frames.iter().any(|(_, p)| p.len() > u32::MAX as usize) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }
        let size: u64 = frames
            .iter()
            .map(|(_, p)| HEADER + p.len() as u64)
            .sum::<u64>()
            + HEADER
            + MARKER_MAX;
        if may_roll && self.len > 0 && self.len + size > self.max_len {
            self.roll()?;
        }
        let mut placed = Vec::with_capacity(frames.len());
        for (kind, payload) in frames {
            placed.push(self.write_frame(*kind, payload)?);
        }
        Ok(placed)
    }

    /// Writes the `BATCH_COMMIT` marker of the batch whose first data frame is at `start` and
    /// whose data frames have `hashes`, in order. It does not sync.
    pub fn append_marker<'a>(
        &mut self,
        start: Location,
        hashes: impl ExactSizeIterator<Item = &'a Hash>,
    ) -> io::Result<()> {
        let marker = BatchCommit {
            start,
            frames: hashes.len() as u32,
            digest: kinds::batch_digest(hashes),
        };
        let bytes = postcard::to_allocvec(&marker).expect("postcard encoding of a marker");
        debug_assert!(bytes.len() as u64 <= MARKER_MAX);
        self.write_frame(kinds::BATCH_COMMIT, &bytes)?;
        Ok(())
    }

    /// Test hook: arms `p` (or disarms with None). It stays armed until it fires.
    #[doc(hidden)]
    pub fn crash_at(&mut self, p: Option<CrashPoint>) {
        self.crash = p;
    }

    /// Test hook: aborts the process (no destructors) if `p` is the armed crash point.
    #[doc(hidden)]
    pub fn crash_point(&self, p: CrashPoint) {
        if self.crash == Some(p) {
            std::process::abort();
        }
    }

    /// Test hook: changes the segment size limit.
    #[doc(hidden)]
    pub fn set_max_len(&mut self, max: u64) {
        self.max_len = max;
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.writer.sync_data()
    }

    /// Reads the frame at `loc` with the payload hash its header carries (already verified).
    pub fn read_hashed(&self, loc: Location) -> io::Result<(u8, Hash, Vec<u8>)> {
        let f = self
            .readers
            .get(loc.segment as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such segment"))?;
        let file_len = f.metadata()?.len();
        read_frame(f, loc.offset, file_len)?.ok_or_else(|| corrupt_frame(loc))
    }

    pub fn read(&self, loc: Location) -> io::Result<(u8, Vec<u8>)> {
        let (kind, _, payload) = self.read_hashed(loc)?;
        Ok((kind, payload))
    }

    /// The location, kind and hash of every frame from `from` to the end of the log. A thin
    /// collector over `for_each_from`, which owns the torn-frame rule.
    pub fn scan_from(&self, from: Location) -> io::Result<Vec<(Location, u8, Hash)>> {
        let mut out = vec![];
        self.for_each_from(from, |loc, kind, hash, _, _| -> io::Result<()> {
            out.push((loc, kind, hash));
            Ok(())
        })?;
        Ok(out)
    }

    /// The length of segment `seg`'s file.
    pub fn segment_len(&self, seg: u32) -> io::Result<u64> {
        let f = self
            .readers
            .get(seg as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such segment"))?;
        Ok(f.metadata()?.len())
    }

    /// Calls `f(location, kind, hash, payload, next)` for every frame from `from` to the end of
    /// the log, in order, reading each frame once; `next` is the location just past the frame. A
    /// `from` at the end of its segment continues with the next segment. A torn or corrupt frame
    /// is `InvalidData`, as is a `from` past the end of its segment and a sealed segment (one
    /// before the current) that does not end at a batch marker. An error from `f` stops the walk
    /// and is returned.
    pub fn for_each_from<E: From<io::Error>>(
        &self,
        from: Location,
        mut f: impl FnMut(Location, u8, Hash, &[u8], Location) -> Result<(), E>,
    ) -> Result<(), E> {
        for seg in from.segment..=self.current() {
            let file = &self.readers[seg as usize];
            let file_len = file.metadata()?.len();
            let mut offset = if seg == from.segment { from.offset } else { 0 };
            if offset > file_len {
                // `from` lies past the end of its segment: frames before it are missing
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt frame in segment {seg} at offset {offset}"),
                )
                .into());
            }
            let sealed = seg < self.current();
            if sealed && offset == 0 && file_len == 0 {
                return Err(unsealed(seg).into());
            }
            while let Some((kind, hash, payload)) = read_frame(file, offset, file_len)? {
                let loc = Location {
                    segment: seg,
                    offset,
                };
                offset += HEADER + payload.len() as u64;
                let next = Location {
                    segment: seg,
                    offset,
                };
                if sealed && offset == file_len && kind != kinds::BATCH_COMMIT {
                    return Err(unsealed(seg).into());
                }
                f(loc, kind, hash, &payload, next)?;
            }
            if offset < file_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt frame in segment {seg} at offset {offset}"),
                )
                .into());
            }
        }
        Ok(())
    }

    /// Reads every frame of every segment once and verifies its hash and every batch marker, and
    /// that every sealed segment (one before the current) ends at a batch marker. Returns the
    /// number of non-marker frames.
    pub fn verify_frames(&self) -> io::Result<usize> {
        let mut count = 0;
        for seg in 0..=self.current() {
            let f = &self.readers[seg as usize];
            let file_len = f.metadata()?.len();
            let mut offset = 0;
            // the frames read since the previous valid marker of this segment
            let mut since: Vec<(u64, Hash)> = vec![];
            while let Some((kind, hash, payload)) = read_frame(f, offset, file_len)? {
                if kind == kinds::BATCH_COMMIT {
                    let mismatch = || {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "batch marker in segment {seg} at offset {offset} does not match its batch"
                            ),
                        )
                    };
                    if !marker_matches(seg, &since, &payload) {
                        return Err(mismatch());
                    }
                    since.clear();
                } else {
                    since.push((offset, hash));
                    count += 1;
                }
                offset += HEADER + payload.len() as u64;
            }
            if offset < file_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("corrupt frame in segment {seg} at offset {offset}"),
                ));
            }
            if seg < self.current() && (file_len == 0 || !since.is_empty()) {
                return Err(unsealed(seg));
            }
        }
        Ok(count)
    }
}
