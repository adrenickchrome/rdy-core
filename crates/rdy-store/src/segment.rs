use crate::error::StoreError;
use crate::fsutil::sync_dir;
use crate::kinds::{self, BatchCommit};
use crate::pio;
use crate::redaction_io::{self, Authorization, ERASE_BUFFER, Failpoint};
use rdy_model::Hash;
use rdy_model::redaction::DropSpan;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub(crate) const HEADER: u64 = 4 + 1 + 32;
/// An upper bound on the encoded `BatchCommit` payload, used to size a batch before it is written.
pub(crate) const MARKER_MAX: u64 = 64;

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

/// A frame whose payload an authorized redaction removes: the facts its header keeps. A scan
/// accepts the frame at its location when its header still says exactly this, without reading
/// or hashing its payload (which is zero, partly zero, or not yet erased).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hole {
    pub kind: u8,
    pub hash: Hash,
    pub len: u32,
}

/// The authorized holes of a log, by frame location.
pub type Holes = BTreeMap<Location, Hole>;

/// No holes: a log no redaction apply authorizes anything in.
pub(crate) static NO_HOLES: Holes = BTreeMap::new();

/// One frame as a hole-aware read sees it.
pub(crate) enum FrameRead {
    /// A frame whose payload matches its hash.
    Whole(u8, Hash, Vec<u8>),
    /// An authorized hole: kind, hash and payload length from its header; the payload is not read.
    Hole(u8, Hash, u32),
}

impl FrameRead {
    fn payload_len(&self) -> u64 {
        match self {
            FrameRead::Whole(_, _, p) => p.len() as u64,
            FrameRead::Hole(_, _, len) => u64::from(*len),
        }
    }

    fn kind_hash(&self) -> (u8, Hash) {
        match self {
            FrameRead::Whole(k, h, _) | FrameRead::Hole(k, h, _) => (*k, *h),
        }
    }
}

/// One frame handed to a hole-aware walk: its header facts and, unless it is an authorized hole,
/// its payload.
#[derive(Clone, Copy, Debug)]
pub struct FrameView<'a> {
    pub kind: u8,
    pub hash: Hash,
    pub len: u32,
    /// None for an authorized hole: its payload is never read nor handed out.
    pub payload: Option<&'a [u8]>,
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
    /// The frames the next appends write whole before `fail_partial` fires.
    fail_skip: usize,
    /// Test hook: the armed crash point (see `crash_at`).
    crash: Option<CrashPoint>,
    /// What the log's validated redaction applies authorize: the holes every scan and walk of
    /// this log reads as frames (`redaction_io::bootstrap`).
    auth: Arc<Authorization>,
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

/// A test observer of segment writes: it sees each byte slice a log append is about to write,
/// before the write, and keeps nothing of it unless it chooses to. An error it returns is the
/// write's (a test fault): nothing of that slice is written.
#[doc(hidden)]
pub type WriteObserver = Box<dyn FnMut(&[u8]) -> io::Result<()>>;

thread_local! {
    static WRITE_OBSERVER: std::cell::RefCell<Option<WriteObserver>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether any thread has set a write observer: the write path skips the thread-local lookup
/// until one is set.
static OBSERVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test hook: every log append on this thread (frames, markers, a dropped frame's header and
/// zeros) calls `observer` with each byte slice right before it is written; an error it returns
/// fails that write before any of its bytes is written. None removes it.
#[doc(hidden)]
pub fn on_segment_write(observer: Option<WriteObserver>) {
    if observer.is_some() {
        OBSERVED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    WRITE_OBSERVER.with(|o| *o.borrow_mut() = observer);
}

/// Shows `bytes`, about to be written to a segment, to this thread's write observer, and
/// returns its verdict.
fn observe(bytes: &[u8]) -> io::Result<()> {
    if OBSERVED.load(std::sync::atomic::Ordering::Relaxed) {
        return WRITE_OBSERVER.with(|o| match o.borrow_mut().as_mut() {
            Some(f) => f(bytes),
            None => Ok(()),
        });
    }
    Ok(())
}

/// A positioned write of segment bytes, shown first to the write observer.
fn write_observed(f: &File, bytes: &[u8], at: u64) -> io::Result<()> {
    observe(bytes)?;
    pio::write_all_at(f, bytes, at)
}

/// Reads the frame at `offset`, or returns None if it is torn or corrupt.
fn read_frame(f: &File, offset: u64, file_len: u64) -> io::Result<Option<(u8, Hash, Vec<u8>)>> {
    let Some(payload_start) = offset.checked_add(HEADER).filter(|end| *end <= file_len) else {
        return Ok(None);
    };
    let mut header = [0u8; HEADER as usize];
    pio::read_exact_at(f, &mut header, offset)?;
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
    pio::read_exact_at(f, &mut payload, payload_start)?;
    count_hashed(payload.len());
    if Hash::of(&payload) != hash {
        return Ok(None);
    }
    Ok(Some((kind, hash, payload)))
}

/// The header of the frame at `offset` (payload length, kind, hash), if the header and the
/// payload it claims fit before `file_len` (checked arithmetic); None for a torn frame.
pub(crate) fn header_at(
    f: &File,
    offset: u64,
    file_len: u64,
) -> io::Result<Option<(u32, u8, Hash)>> {
    let Some(payload_start) = offset.checked_add(HEADER).filter(|end| *end <= file_len) else {
        return Ok(None);
    };
    let mut header = [0u8; HEADER as usize];
    pio::read_exact_at(f, &mut header, offset)?;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if payload_start
        .checked_add(u64::from(len))
        .is_none_or(|end| end > file_len)
    {
        return Ok(None);
    }
    Ok(Some((
        len,
        header[4],
        Hash(header[5..37].try_into().unwrap()),
    )))
}

/// Reads the frame at `loc` as `read_frame` does, except that an authorized hole whose header
/// still names its kind, hash and length is returned as a hole, with its payload neither read nor
/// hashed. None for a torn or corrupt frame.
fn read_frame_or_hole(
    f: &File,
    loc: Location,
    file_len: u64,
    holes: &Holes,
) -> io::Result<Option<FrameRead>> {
    if let Some(h) = holes.get(&loc)
        && header_at(f, loc.offset, file_len)? == Some((h.len, h.kind, h.hash))
    {
        return Ok(Some(FrameRead::Hole(h.kind, h.hash, h.len)));
    }
    Ok(read_frame(f, loc.offset, file_len)?.map(|(k, h, p)| FrameRead::Whole(k, h, p)))
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
    pio::read_exact_at(f, &mut buf, file_len - tail)?;
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

/// A whole frame whose payload is all zero at a location no redaction authorizes: a missing
/// payload (E_REDACTION_CORRUPT). The read paths return `io::Error`s, so they carry it inside
/// one (`refuse_frame`), with the exact `E_REDACTION_CORRUPT` text; `StoreError::from` turns it
/// back into `StoreError::RedactionCorrupt` at that location.
#[derive(Debug)]
pub(crate) struct UnauthorizedHole(pub(crate) Location);

impl std::fmt::Display for UnauthorizedHole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        StoreError::corrupt_at(self.0).fmt(f)
    }
}

impl std::error::Error for UnauthorizedHole {}

/// The refusal of a read of the frame at `loc` (file `f`, `file_len` bytes) that does not fit its
/// file or match its hash: an all-zero payload (a hole no redaction authorizes; the caller read
/// the authorized holes as holes) is `UnauthorizedHole`; any other damage is `otherwise`.
fn refuse_frame(f: &File, loc: Location, file_len: u64, otherwise: io::Error) -> io::Error {
    if zero_frame_at(f, loc.offset, file_len).unwrap_or(false) {
        return io::Error::new(io::ErrorKind::InvalidData, UnauthorizedHole(loc));
    }
    otherwise
}

/// The refusal of damage the redaction bootstrap met at `offset` of sealed segment `seg` (a torn
/// frame header, or a batch marker that does not match its batch): the frames behind it cannot
/// be validated, so the open stops. A whole frame there with an all-zero payload is
/// `RedactionCorrupt`, as any unauthorized hole is; other damage is an I/O refusal.
pub(crate) fn sealed_damage(f: &File, seg: u32, offset: u64, file_len: u64) -> StoreError {
    if zero_frame_at(f, offset, file_len).unwrap_or(false) {
        return StoreError::RedactionCorrupt {
            segment: seg,
            offset,
        };
    }
    StoreError::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "corruption at offset {offset} in segment {seg} damages acknowledged data: a later segment was started, but a frame header or batch marker there is broken; restore the segment from backup, then reopen"
        ),
    ))
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

    /// The header of the frame at `loc` (payload length, kind, hash), read alone; None when the
    /// header or the payload it claims does not fit its file.
    pub(crate) fn header(&self, loc: Location) -> io::Result<Option<(u32, u8, Hash)>> {
        let f = self.file(loc.segment)?;
        header_at(&f, loc.offset, f.metadata()?.len())
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
        self.for_each_view_from(from, upto, &NO_HOLES, |loc, v, next| match v.payload {
            Some(p) => f(loc, v.kind, v.hash, p, next),
            None => unreachable!("a walk with no holes reads every payload"),
        })
    }

    /// `for_each_from`, reading each frame of `holes` whose header still names its kind, hash and
    /// length as a [`FrameView`] with no payload: its payload is neither read nor hashed.
    pub(crate) fn for_each_view_from<E: From<io::Error>>(
        &self,
        from: Location,
        upto: Location,
        holes: &Holes,
        mut f: impl FnMut(Location, FrameView<'_>, Location) -> Result<(), E>,
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
            let file_len = file.metadata()?.len();
            while offset < end {
                let loc = Location {
                    segment: seg,
                    offset,
                };
                let frame = read_frame_or_hole(&file, loc, file_len, holes)?
                    .ok_or_else(|| refuse_frame(&file, loc, file_len, corrupt_frame(loc)))?;
                let next = offset + HEADER + frame.payload_len();
                if next > end {
                    return Err(corrupt_frame(loc).into());
                }
                let next = Location {
                    segment: seg,
                    offset: next,
                };
                let (kind, hash) = frame.kind_hash();
                if sealed && next.offset == end && kind != kinds::BATCH_COMMIT {
                    return Err(unsealed(seg).into());
                }
                let view = match &frame {
                    FrameRead::Whole(_, _, p) => FrameView {
                        kind,
                        hash,
                        len: p.len() as u32,
                        payload: Some(p),
                    },
                    FrameRead::Hole(_, _, len) => FrameView {
                        kind,
                        hash,
                        len: *len,
                        payload: None,
                    },
                };
                f(loc, view, next)?;
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
    let (kind, hash, payload) = read_frame(f, loc.offset, file_len)?
        .ok_or_else(|| refuse_frame(f, loc, file_len, corrupt_frame(loc)))?;
    let next = loc.offset + HEADER + payload.len() as u64;
    Ok((kind, hash, payload, next))
}

/// True if any valid frame (header parses, fits within EOF, payload hash verifies)
/// starts after `offset`. A genuine torn tail never has one.
fn valid_frame_after(f: &File, offset: u64, file_len: u64) -> io::Result<bool> {
    let mut buf = vec![0u8; (file_len - offset) as usize];
    pio::read_exact_at(f, &mut buf, offset)?;
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
    // the tail is copied through one fixed buffer, whatever its length
    let mut buf = vec![0u8; ERASE_BUFFER];
    let mut at = offset;
    while at < file_len {
        let n = (file_len - at).min(ERASE_BUFFER as u64) as usize;
        pio::read_exact_at(f, &mut buf[..n], at)?;
        pio::write_all_at(&q, &buf[..n], at - offset)?;
        at += n as u64;
    }
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
pub(crate) fn marker_matches(seg: u32, since: &[(u64, Hash)], payload: &[u8]) -> bool {
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

/// The marker rule's scan of segment `seg`. An authorized hole (`holes`) is a frame like any
/// other: its header's hash enters the batch digest, and its payload is not hashed.
fn scan_marked(f: &File, seg: u32, file_len: u64, holes: &Holes) -> io::Result<Scan> {
    let mut offset = 0;
    let mut boundary = None;
    let mut since: Vec<(u64, Hash)> = vec![];
    while let Some(frame) = read_frame_or_hole(
        f,
        Location {
            segment: seg,
            offset,
        },
        file_len,
        holes,
    )? {
        let end = offset + HEADER + frame.payload_len();
        match frame {
            FrameRead::Whole(kind, _, payload) if kind == kinds::BATCH_COMMIT => {
                if !marker_matches(seg, &since, &payload) {
                    break;
                }
                since.clear();
                boundary = Some(end);
            }
            other => since.push((offset, other.kind_hash().1)),
        }
        offset = end;
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
fn tail_frame(
    f: &File,
    seg: u32,
    offset: u64,
    bound: u64,
    memo: &mut TailMemo,
    holes: &Holes,
) -> io::Result<Step> {
    if let Some(h) = holes.get(&Location {
        segment: seg,
        offset,
    }) && let Some((len, kind, hash)) = header_at(f, offset, bound)?
        && (len, kind, hash) == (h.len, h.kind, h.hash)
    {
        // an authorized hole: its header is the frame; its payload is never hashed
        return Ok(Step::Frame(kind, hash, offset + HEADER + u64::from(len)));
    }
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
    pio::read_exact_at(f, &mut header, offset)?;
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as u64;
    let Some(end) = payload_start.checked_add(len).filter(|end| *end <= bound) else {
        return Ok(Step::PastBound);
    };
    let hash = Hash(header[5..37].try_into().unwrap());
    let mut payload = vec![0u8; len as usize];
    pio::read_exact_at(f, &mut payload, payload_start)?;
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
    holes: &Holes,
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
        match tail_frame(f, seg, offset, marker_at, memo, holes)? {
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
    holes: &Holes,
) -> io::Result<bool> {
    let mut buf = vec![0u8; (file_len - stop) as usize];
    pio::read_exact_at(f, &mut buf, stop)?;
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
            && closes_a_real_batch(f, seg, stop + pos as u64, payload, &mut memo, holes)?
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
    holes: &Holes,
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
        || acknowledged_batch_follows(f, seg, b, scan.stop, file_len, holes)?
    {
        return Err(damaged_error(scan.stop, &path));
    }
    quarantine(f, dir, &path, b, file_len).map(Some)
}

/// The end of the acknowledged log under the marker rule, reading `holes` as frames: the end of
/// the last segment's last batch marker that matches its batch; with no such marker, the end of
/// the segment before it (or the start of the log). Reads only; the open does the same scan
/// before it quarantines anything.
pub(crate) fn acknowledged_end(readers: &[File], holes: &Holes) -> io::Result<Location> {
    let last = (readers.len() - 1) as u32;
    let f = &readers[last as usize];
    let scan = scan_marked(f, last, f.metadata()?.len(), holes)?;
    Ok(match scan.boundary {
        Some(offset) => Location {
            segment: last,
            offset,
        },
        None if last > 0 => Location {
            segment: last - 1,
            offset: readers[last as usize - 1].metadata()?.len(),
        },
        None => Location {
            segment: 0,
            offset: 0,
        },
    })
}

/// True if the frame at `offset` is whole by its header and its payload is all zero bytes: what an
/// erased payload looks like.
fn zero_frame_at(f: &File, offset: u64, file_len: u64) -> io::Result<bool> {
    let Some((len, _, _)) = header_at(f, offset, file_len)? else {
        return Ok(false);
    };
    Ok(len > 0 && payload_is_zero(f, offset + HEADER, u64::from(len))?)
}

/// True if the `len` bytes at `start` of `f` are all zero, read through one fixed buffer.
pub(crate) fn payload_is_zero(f: &File, start: u64, len: u64) -> io::Result<bool> {
    let mut buf = vec![0u8; ERASE_BUFFER];
    let mut at = start;
    let end = start + len;
    while at < end {
        let n = (end - at).min(ERASE_BUFFER as u64) as usize;
        pio::read_exact_at(f, &mut buf[..n], at)?;
        if buf[..n].iter().any(|b| *b != 0) {
            return Ok(false);
        }
        at += n as u64;
    }
    Ok(true)
}

/// The refusal of damage at `stop` of segment `seg`. A missing payload no redaction authorizes
/// (a whole frame whose payload is all zero) is `RedactionCorrupt` at that frame; other damage
/// keeps its refusal.
fn damage_refusal(e: io::Error, f: &File, seg: u32, stop: u64, file_len: u64) -> StoreError {
    if e.kind() == io::ErrorKind::InvalidData && zero_frame_at(f, stop, file_len).unwrap_or(false) {
        return StoreError::RedactionCorrupt {
            segment: seg,
            offset: stop,
        };
    }
    StoreError::Io(e)
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
        if offset < file_len && acknowledged_batch_follows(&f, seg, 0, offset, file_len, &NO_HOLES)?
        {
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
            fail_skip: 0,
            crash: None,
            auth: Arc::default(),
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
        SegmentLog::open_authorized(dir, max_len, floor).map_err(|e| match e {
            StoreError::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        })
    }

    /// `open_marked`, with the redaction bootstrap's typed refusals. The open makes two passes
    /// and never consults the index for authorization: the first reads frame headers and markers
    /// and validates every sealed redaction apply with its request, residues and same-batch human
    /// landing (`redaction_io::bootstrap`); the second is the marker rule's scan, which hashes
    /// every payload it reads except the authorized holes. A whole frame with an all-zero payload
    /// that no apply authorizes is `RedactionCorrupt` at it; a forged or broken apply is
    /// `RedactionCorrupt` too. Nothing is erased here: `Store::finish_redactions` does that.
    pub(crate) fn open_authorized(
        dir: &Path,
        max_len: u64,
        floor: Option<Location>,
    ) -> Result<(SegmentLog, Vec<PathBuf>), StoreError> {
        let mut readers = open_readers(dir)?;
        // the mode the first batch sets decides the bootstrap: a holder's applies are checked by
        // their logical tuples (`redaction_import`), a project's by their own spans' places
        let auth = Arc::new(match crate::redaction_import::log_mode(&readers)? {
            crate::holder::StoreMode::Holder => {
                crate::redaction_import::bootstrap_holder(&readers)?
            }
            crate::holder::StoreMode::Project => redaction_io::bootstrap(&readers)?,
        });
        let holes = &auth.holes;
        let mut last = (readers.len() - 1) as u32;
        let mut writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(seg_path(dir, last))?;
        let file_len = writer.metadata()?.len();
        let scan = scan_marked(&writer, last, file_len, holes)?;
        let mut quarantined = vec![];
        let len;
        if scan.boundary.is_none() && last > 0 {
            // the last segment is one unacknowledged batch, if the one before it is sealed at a marker
            let prev_path = seg_path(dir, last - 1);
            let prev = File::open(&prev_path)?;
            let prev_len = prev.metadata()?.len();
            if scan_marked(&prev, last - 1, prev_len, holes)?.boundary != Some(prev_len) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} does not end at a batch marker, but a later segment was started: the log is damaged; restore it from backup, then reopen",
                        prev_path.display()
                    ),
                )
                .into());
            }
            quarantined.extend(
                check_tail(&writer, dir, last, &scan, file_len, floor, holes)
                    .map_err(|e| damage_refusal(e, &writer, last, scan.stop, file_len))?,
            );
            drop(writer);
            std::fs::remove_file(seg_path(dir, last))?;
            sync_dir(dir)?;
            readers.pop();
            last -= 1;
            writer = OpenOptions::new().read(true).write(true).open(&prev_path)?;
            len = prev_len;
        } else {
            let b = scan.boundary.unwrap_or(0);
            if let Some(q) = check_tail(&writer, dir, last, &scan, file_len, floor, holes)
                .map_err(|e| damage_refusal(e, &writer, last, scan.stop, file_len))?
            {
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
            fail_skip: 0,
            crash: None,
            auth,
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
        self.fail_skip = 0;
    }

    /// Test hook: the appends write `frames` frames whole, then only the first `bytes` bytes
    /// of the next one, and fail; `None` disarms it.
    #[doc(hidden)]
    pub fn fail_append_at_frame(&mut self, at: Option<(usize, usize)>) {
        self.fail_partial = at.map(|(_, bytes)| bytes);
        self.fail_skip = at.map_or(0, |(frames, _)| frames);
    }

    /// Starts segment `current + 1`. The new writer, reader and length are committed together,
    /// only once the file and the directory are durable; on any failure the log is exactly as
    /// before and the new file is removed best-effort (`truncate_to` sweeps a leftover next segment).
    pub(crate) fn roll(&mut self) -> io::Result<()> {
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
        if self.fail_partial.is_some() && self.fail_skip > 0 {
            self.fail_skip -= 1;
        } else if let Some(n) = self.fail_partial.take() {
            write_observed(&self.writer, &buf[..n.min(buf.len())], self.len)?;
            return Err(io::Error::other("injected partial append"));
        }
        write_observed(&self.writer, &buf, self.len)?;
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

    /// Rolls the segment first when a batch of `size` bytes (its frames and its marker) would not
    /// fit in the current one, as `append_frames` does for the first frames of a batch. A batch
    /// that `append_unrolled` writes frame by frame calls it once before its first frame.
    pub(crate) fn roll_for_batch(&mut self, size: u64) -> io::Result<()> {
        if self.len > 0 && self.len.saturating_add(size) > self.max_len {
            self.roll()?;
        }
        Ok(())
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

    /// Writes one data frame of a loaded batch at the end of the current segment, with no roll
    /// and no sync: the ingest rolls only where the source rolled (`crate::ingest`). It
    /// allocates nothing (`write_parts`). Returns where it went and its payload's hash.
    pub(crate) fn append_unrolled(
        &mut self,
        kind: u8,
        payload: &[u8],
    ) -> io::Result<(Location, Hash)> {
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload too large"))?;
        let hash = Hash::of(payload);
        let loc = self.write_parts(len, kind, &hash, payload)?;
        Ok((loc, hash))
    }

    /// Writes the `BATCH_COMMIT` marker of a loaded batch whose first data frame is at `start`,
    /// of `frames` data frames whose batch digest is `digest`, with no sync. It allocates
    /// nothing: the marker is encoded into a fixed buffer of `MARKER_MAX` bytes. The bytes are
    /// those `append_marker` writes for the same batch.
    pub(crate) fn append_marker_digest(
        &mut self,
        start: Location,
        frames: u32,
        digest: Hash,
    ) -> io::Result<()> {
        let marker = BatchCommit {
            start,
            frames,
            digest,
        };
        let mut buf = [0u8; MARKER_MAX as usize];
        let bytes = postcard::to_slice(&marker, &mut buf)
            .map_err(|_| io::Error::other("a marker over its bound"))?;
        let hash = Hash::of(bytes);
        self.write_parts(bytes.len() as u32, kinds::BATCH_COMMIT, &hash, bytes)?;
        Ok(())
    }

    /// Writes one frame (`len`, `kind`, `hash`, then `payload`) at the end of the current
    /// segment with no roll and no sync, and allocates nothing: a frame of at most
    /// `SMALL_FRAME` bytes goes through one buffer on the stack in one write, a longer one as
    /// its header and then its payload.
    fn write_parts(
        &mut self,
        len: u32,
        kind: u8,
        hash: &Hash,
        payload: &[u8],
    ) -> io::Result<Location> {
        const SMALL_FRAME: usize = 4096;
        let loc = Location {
            segment: self.current(),
            offset: self.len,
        };
        let head_len = HEADER as usize;
        let frame_len = head_len + payload.len();
        let mut small = [0u8; SMALL_FRAME];
        small[0..4].copy_from_slice(&len.to_le_bytes());
        small[4] = kind;
        small[5..head_len].copy_from_slice(&hash.0);
        if frame_len <= SMALL_FRAME {
            small[head_len..frame_len].copy_from_slice(payload);
            write_observed(&self.writer, &small[..frame_len], self.len)?;
        } else {
            write_observed(&self.writer, &small[..head_len], self.len)?;
            write_observed(&self.writer, payload, self.len + HEADER)?;
        }
        self.len += frame_len as u64;
        Ok(loc)
    }

    /// Writes a loaded frame whose payload a redaction removed, at the end of the current
    /// segment with no roll and no sync: its original header (`len`, `kind`, `hash`), then `len`
    /// zero bytes through one fixed buffer of `ERASE_BUFFER` bytes, by positioned writes. So the
    /// frame takes its original place and length, as an erased frame of the source does.
    /// Returns where it went.
    pub(crate) fn append_dropped(
        &mut self,
        kind: u8,
        hash: &Hash,
        len: u32,
    ) -> io::Result<Location> {
        static ZEROS: [u8; ERASE_BUFFER] = [0; ERASE_BUFFER];
        let loc = Location {
            segment: self.current(),
            offset: self.len,
        };
        let end = self
            .len
            .checked_add(HEADER)
            .and_then(|e| e.checked_add(u64::from(len)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
        let mut head = [0u8; HEADER as usize];
        head[0..4].copy_from_slice(&len.to_le_bytes());
        head[4] = kind;
        head[5..37].copy_from_slice(&hash.0);
        write_observed(&self.writer, &head, self.len)?;
        let mut at = self.len + HEADER;
        while at < end {
            let n = (end - at).min(ERASE_BUFFER as u64) as usize;
            write_observed(&self.writer, &ZEROS[..n], at)?;
            at += n as u64;
        }
        self.len = end;
        Ok(loc)
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
        read_frame(f, loc.offset, file_len)?
            .ok_or_else(|| refuse_frame(f, loc, file_len, corrupt_frame(loc)))
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
    /// before the current) that does not end at a batch marker. An authorized hole is
    /// `InvalidData` too: this walk hands out only original payloads (`for_each_view` walks past
    /// holes). An error from `f` stops the walk and is returned.
    pub fn for_each_from<E: From<io::Error>>(
        &self,
        from: Location,
        mut f: impl FnMut(Location, u8, Hash, &[u8], Location) -> Result<(), E>,
    ) -> Result<(), E> {
        self.for_each_view(from, |loc, v, next| match v.payload {
            Some(p) => f(loc, v.kind, v.hash, p, next),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the payload of the frame at {loc:?} was removed by a redaction"),
            )
            .into()),
        })
    }

    /// `for_each_from`, handing each frame as a [`FrameView`]: an authorized hole of this log
    /// comes with its header facts and no payload, which is neither read nor hashed.
    pub fn for_each_view<E: From<io::Error>>(
        &self,
        from: Location,
        mut f: impl FnMut(Location, FrameView<'_>, Location) -> Result<(), E>,
    ) -> Result<(), E> {
        let holes = &self.auth.holes;
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
            while let Some(frame) = read_frame_or_hole(
                file,
                Location {
                    segment: seg,
                    offset,
                },
                file_len,
                holes,
            )? {
                let loc = Location {
                    segment: seg,
                    offset,
                };
                offset += HEADER + frame.payload_len();
                let next = Location {
                    segment: seg,
                    offset,
                };
                let (kind, hash) = frame.kind_hash();
                if sealed && offset == file_len && kind != kinds::BATCH_COMMIT {
                    return Err(unsealed(seg).into());
                }
                let view = match &frame {
                    FrameRead::Whole(_, _, p) => FrameView {
                        kind,
                        hash,
                        len: p.len() as u32,
                        payload: Some(p),
                    },
                    FrameRead::Hole(_, _, len) => FrameView {
                        kind,
                        hash,
                        len: *len,
                        payload: None,
                    },
                };
                f(loc, view, next)?;
            }
            if offset < file_len {
                let loc = Location {
                    segment: seg,
                    offset,
                };
                return Err(refuse_frame(
                    file,
                    loc,
                    file_len,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("corrupt frame in segment {seg} at offset {offset}"),
                    ),
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
        Ok(self.verify_frames_counted()?.0)
    }

    /// `verify_frames`, reading the log's authorized holes as frames whose payloads are neither
    /// read nor hashed. Returns the non-marker frames and, of them, the holes.
    pub(crate) fn verify_frames_counted(&self) -> io::Result<(usize, usize)> {
        let holes = &self.auth.holes;
        let mut count = 0;
        let mut dropped = 0;
        for seg in 0..=self.current() {
            let f = &self.readers[seg as usize];
            let file_len = f.metadata()?.len();
            let mut offset = 0;
            // the frames read since the previous valid marker of this segment
            let mut since: Vec<(u64, Hash)> = vec![];
            while let Some(frame) = read_frame_or_hole(
                f,
                Location {
                    segment: seg,
                    offset,
                },
                file_len,
                holes,
            )? {
                let end = offset + HEADER + frame.payload_len();
                match frame {
                    FrameRead::Whole(kind, _, payload) if kind == kinds::BATCH_COMMIT => {
                        if !marker_matches(seg, &since, &payload) {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "batch marker in segment {seg} at offset {offset} does not match its batch"
                                ),
                            ));
                        }
                        since.clear();
                    }
                    other => {
                        if matches!(other, FrameRead::Hole(..)) {
                            dropped += 1;
                        }
                        since.push((offset, other.kind_hash().1));
                        count += 1;
                    }
                }
                offset = end;
            }
            if offset < file_len {
                let loc = Location {
                    segment: seg,
                    offset,
                };
                return Err(refuse_frame(
                    f,
                    loc,
                    file_len,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("corrupt frame in segment {seg} at offset {offset}"),
                    ),
                ));
            }
            if seg < self.current() && (file_len == 0 || !since.is_empty()) {
                return Err(unsealed(seg));
            }
        }
        Ok((count, dropped))
    }

    /// What the log's validated redaction applies authorize.
    pub(crate) fn authorization(&self) -> &Arc<Authorization> {
        &self.auth
    }

    /// Replaces the authorization, once a newly sealed apply batch is durable.
    pub(crate) fn set_authorization(&mut self, auth: Arc<Authorization>) {
        self.auth = auth;
    }

    /// The readers of every segment, in order (for the redaction bootstrap of a pending batch).
    pub(crate) fn readers(&self) -> &[File] {
        &self.readers
    }

    /// True if a frame (or an authorized hole) starts at `loc`.
    pub(crate) fn frame_readable(&self, loc: Location) -> io::Result<bool> {
        let Some(f) = self.readers.get(loc.segment as usize) else {
            return Ok(false);
        };
        Ok(read_frame_or_hole(f, loc, f.metadata()?.len(), &self.auth.holes)?.is_some())
    }

    /// Where `append_batch` would put the first frame of `frames`: the end of the log, or the start
    /// of the next segment when the batch and its marker do not fit in the current one.
    pub(crate) fn batch_start(&self, frames: &[(u8, Vec<u8>)]) -> Location {
        let size = frames
            .iter()
            .map(|(_, p)| HEADER + p.len() as u64)
            .sum::<u64>()
            + HEADER
            + MARKER_MAX;
        if self.len > 0 && self.len + size > self.max_len {
            Location {
                segment: self.current() + 1,
                offset: 0,
            }
        } else {
            self.end()
        }
    }

    /// The state of the payload of hole `loc`: all zero (`Some(true)` erased), intact and matching
    /// its hash (`Some(false)`), or neither (`None`: partly erased).
    pub(crate) fn hole_state(&self, loc: Location, hole: &Hole) -> io::Result<Option<bool>> {
        let f = self
            .readers
            .get(loc.segment as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such segment"))?;
        let start = loc.offset + HEADER;
        if payload_is_zero(f, start, u64::from(hole.len))? {
            return Ok(Some(true));
        }
        let intact = read_frame(f, loc.offset, f.metadata()?.len())?.is_some_and(|(k, h, p)| {
            k == hole.kind && h == hole.hash && p.len() == hole.len as usize
        });
        Ok(intact.then_some(false))
    }

    /// Overwrites the payload of the frame `span` names with zeros, through one fixed buffer of
    /// `ERASE_BUFFER` bytes, by positioned writes. It first checks that the frame's header still
    /// holds the span's kind, hash and length and that the payload ends inside the file (checked
    /// arithmetic); anything else is `RedactionCorrupt` at the span, and nothing is written. The
    /// header is never rewritten and the file never truncated. It does not sync
    /// (`sync_segments`).
    pub fn zero_payload(&mut self, span: &DropSpan) -> Result<(), StoreError> {
        static ZEROS: [u8; ERASE_BUFFER] = [0; ERASE_BUFFER];
        let loc = Location {
            segment: span.place.segment,
            offset: span.place.offset,
        };
        let corrupt = || StoreError::corrupt_at(loc);
        if loc.segment > self.current() {
            return Err(corrupt());
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(seg_path(&self.dir, loc.segment))?;
        let file_len = f.metadata()?.len();
        if header_at(&f, loc.offset, file_len)? != Some((span.len, span.kind, span.hash)) {
            return Err(corrupt());
        }
        let start = loc.offset.checked_add(HEADER).ok_or_else(corrupt)?;
        let end = start
            .checked_add(u64::from(span.len))
            .filter(|e| *e <= file_len)
            .ok_or_else(corrupt)?;
        let mut at = start;
        while at < end {
            let n = (end - at).min(ERASE_BUFFER as u64) as usize;
            pio::write_all_at(&f, &ZEROS[..n], at)?;
            at += n as u64;
            redaction_io::hit(Failpoint::AfterZeroWrite)?;
        }
        Ok(())
    }

    /// Makes the payload writes to `segments` durable: one sync of each segment, then of the
    /// segments directory.
    pub(crate) fn sync_segments(&self, segments: &BTreeSet<u32>) -> io::Result<()> {
        for seg in segments {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(seg_path(&self.dir, *seg))?
                .sync_all()?;
        }
        sync_dir(&self.dir)
    }
}
