//! The span map (D-1ER84): the durable record of exact byte runs of one sealed payload that are
//! also held by an earlier sealed payload, so the later copy's disk space can be given back. It
//! never changes a byte that rdy reads, a hash or an id: every read fills a mapped range from its
//! source before the frame's hash check ([`SpanMap::overlay`]).
//!
//! The file is `store/SPANMAP`, beside `segments`. It starts with `RDYSPANS` and the
//! little-endian u32 version 1. Each row is a little-endian u32 byte length, the postcard bytes
//! of one [`SpanMapRecordV1`] and a 32-byte BLAKE3 digest of `b"rdy-span-map-1\0"`, the
//! previous row's digest (32 zero bytes for the first row), the four length bytes and the row
//! bytes. The journal holds no text and no copied payload: only places, kinds, lengths and
//! hashes.
//!
//! A pass appends its `Map` rows and one `Through { end }` row, then syncs the file once. Only
//! then are the rows published to readers, and only then are the mapped target ranges punched.
//! A lost punch is harmless: the durable map supplies the complete logical range.
use crate::error::StoreError;
use crate::segment::{HEADER, Location};
use crate::{fsutil, pio};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};

/// The span map's file name, in the store directory beside `segments`.
pub const SPANMAP_FILE: &str = "SPANMAP";
/// The first bytes of the file.
pub const MAGIC: [u8; 8] = *b"RDYSPANS";
/// The span map's version.
pub const VERSION: u32 = 1;
/// The bytes of the file header: the magic and the version.
pub const FILE_HEADER: u64 = 12;
/// The most postcard bytes of one row.
pub const MAX_ROW_BYTES: usize = 256;
/// The most bytes of the journal: its header and 1,000,000 rows of the largest size.
pub const MAX_JOURNAL_BYTES: u64 = 292_000_012;
/// The most rows of the journal.
pub const MAX_ROWS: u64 = 1_000_000;
/// The unmapped sealed log bytes after which an ordinary write runs one mapping pass.
pub const THRESHOLD_BYTES: u64 = 67_108_864;
/// The shortest run of equal bytes a candidate keeps before alignment.
pub const JOIN_FLOOR: u64 = 256;
/// The mapping alignment on Unix: one whole file system block.
pub const UNIX_ALIGNMENT: u64 = 4096;
/// The most names a torn-tail quarantine tries.
pub const MAX_QUARANTINE_NAMES: u32 = 1_000_000;

/// The domain of a row digest.
const DOMAIN: &[u8] = b"rdy-span-map-1\0";
/// The bytes of a row beside its postcard bytes: the length and the digest.
const ROW_OVERHEAD: u64 = 4 + 32;

/// The full-map note.
pub const FULL_NOTE: &str = "the span map is full; new repeated byte runs keep their disk space";

/// Why a threshold pass stopped mapping on a handle, as its note names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Io,
    Damaged,
    Limit,
}

impl StopReason {
    fn text(self) -> &'static str {
        match self {
            StopReason::Io => "map I/O failed",
            StopReason::Damaged => "map data is damaged",
            StopReason::Limit => "map allocation or row limit failed",
        }
    }

    /// The reason of `e`, by its class.
    pub fn of(e: &StoreError) -> StopReason {
        match e {
            StoreError::DedupCorrupt => StopReason::Damaged,
            StoreError::DedupLimit => StopReason::Limit,
            _ => StopReason::Io,
        }
    }

    /// The note a later compact reports.
    pub fn note(self) -> String {
        format!(
            "span mapping stopped: {}; run rdy compact to retry",
            self.text()
        )
    }
}

/// One byte range of one frame's payload, with the frame's header facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSpan {
    pub place: Location,
    pub raw_kind: u8,
    pub payload_len: u32,
    pub hash: Hash,
    pub start: u32,
    pub len: u32,
}

impl FrameSpan {
    /// The absolute file offset of the range's first byte.
    pub fn physical_start(&self) -> u64 {
        self.place.offset + HEADER + u64::from(self.start)
    }

    /// The location just past the frame.
    pub fn frame_end(&self) -> Location {
        Location {
            segment: self.place.segment,
            offset: self.place.offset + HEADER + u64::from(self.payload_len),
        }
    }

    fn range(&self) -> (u64, u64) {
        let s = u64::from(self.start);
        (s, s + u64::from(self.len))
    }

    /// The range lies inside its payload (checked u32 arithmetic) and is not empty.
    fn in_bounds(&self) -> bool {
        self.len > 0
            && self
                .start
                .checked_add(self.len)
                .is_some_and(|end| end <= self.payload_len)
            && self
                .place
                .offset
                .checked_add(HEADER + u64::from(self.payload_len))
                .is_some()
    }
}

/// One row of the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanMapRecordV1 {
    /// The target range holds exactly the source range's bytes.
    Map {
        target: FrameSpan,
        source: FrameSpan,
    },
    /// The candidate scan of the sealed log through `end` is durable.
    Through { end: Location },
}

/// One mapping: a target range and the source range that holds its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub target: FrameSpan,
    pub source: FrameSpan,
}

/// A step of a mapping pass, as a test observer sees it.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapEvent {
    /// A pass's rows are about to be written: the count of Map rows, and whether a Through
    /// row follows.
    Append { maps: usize, through: bool },
    /// The span map file's one sync of the pass.
    Sync,
    /// The synced rows are published to readers.
    Publish,
    /// A mapped target range is about to be punched.
    Punch(Location),
    /// A candidate scan started.
    Scan,
    /// A threshold pass started.
    Threshold,
}

/// A test observer of mapping steps; an error at `Append` or `Sync` fails that step.
#[doc(hidden)]
pub type MapProbe = Box<dyn FnMut(&MapEvent) -> io::Result<()>>;

thread_local! {
    static MAP_PROBE: RefCell<Option<MapProbe>> = const { RefCell::new(None) };
}

/// Test hook: every mapping step on this thread calls `probe`. None removes it.
#[doc(hidden)]
pub fn on_map_event(probe: Option<MapProbe>) {
    MAP_PROBE.with(|p| *p.borrow_mut() = probe);
}

pub(crate) fn event(e: MapEvent) -> io::Result<()> {
    MAP_PROBE.with(|p| match p.borrow_mut().as_mut() {
        Some(f) => f(&e),
        None => Ok(()),
    })
}

/// Read access to segment files, for a source's header and bytes.
pub(crate) trait SegmentFiles {
    /// The bytes at `offset` of segment `segment`.
    fn read_at(&self, segment: u32, buf: &mut [u8], offset: u64) -> io::Result<()>;
    /// The header (payload length, raw kind, hash) of the frame at `loc`, if it fits its file.
    fn header(&self, loc: Location) -> io::Result<Option<(u32, u8, Hash)>>;
}

impl SegmentFiles for [File] {
    fn read_at(&self, segment: u32, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let f = self
            .get(segment as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such segment"))?;
        pio::read_exact_at(f, buf, offset)
    }

    fn header(&self, loc: Location) -> io::Result<Option<(u32, u8, Hash)>> {
        let Some(f) = self.get(loc.segment as usize) else {
            return Ok(None);
        };
        crate::segment::header_at(f, loc.offset, f.metadata()?.len())
    }
}

/// How an open treats the span map file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapOpen {
    /// A writable store's open: a torn tail is quarantined and cut.
    Writable,
    /// A read-only, port-source or follower open: a torn tail is ignored, and nothing is
    /// written.
    ReadOnly,
}

/// The span map of one store, shared by the log's readers.
#[derive(Default)]
pub struct SpanMap {
    /// The mappings by target frame, each frame's ranges sorted by start.
    targets: BTreeMap<Location, Vec<Mapping>>,
    /// The protected source ranges by source frame, as (start, end) payload offsets.
    sources: BTreeMap<Location, Vec<(u64, u64)>>,
    /// The end of the last durable Through row.
    through: Option<Location>,
    rows: u64,
    /// The valid journal bytes, its header included; 0 when there is no file.
    len: u64,
    digest: [u8; 32],
    /// The file, open for appends (a writable open only).
    file: Mutex<Option<File>>,
    writable: bool,
    /// A torn suffix that a read-only open ignored.
    torn: bool,
    /// The quarantine a writable open wrote.
    quarantined: Option<PathBuf>,
    /// This handle's threshold passes stopped for this reason, until reopen.
    stopped: Option<StopReason>,
    /// A pass found no room for its next complete addition.
    full: bool,
    /// A threshold pass found the map full: no more threshold passes on this handle.
    threshold_off: bool,
}

impl std::fmt::Debug for SpanMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpanMap")
            .field("rows", &self.rows)
            .field("through", &self.through)
            .finish()
    }
}

/// The span map's damage refusal.
fn corrupt() -> StoreError {
    StoreError::DedupCorrupt
}

/// The row digest of `row` after `prev`.
fn row_digest(prev: &[u8; 32], row: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(DOMAIN);
    h.update(prev);
    h.update(&(row.len() as u32).to_le_bytes());
    h.update(row);
    *h.finalize().as_bytes()
}

/// The journal bytes of `record` after digest `prev`, and its digest.
pub(crate) fn encode_row(
    prev: &[u8; 32],
    record: &SpanMapRecordV1,
) -> Result<(Vec<u8>, [u8; 32]), StoreError> {
    let body = postcard::to_allocvec(record).map_err(|_| StoreError::DedupLimit)?;
    if body.len() > MAX_ROW_BYTES {
        return Err(StoreError::DedupLimit);
    }
    let digest = row_digest(prev, &body);
    let mut out = Vec::new();
    out.try_reserve_exact(body.len() + ROW_OVERHEAD as usize)
        .map_err(|_| StoreError::DedupLimit)?;
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&digest);
    Ok((out, digest))
}

/// What one row of the journal is at its offset.
enum RowRead {
    /// A row whose digest chains from the previous one: the record, its digest and its bytes.
    Valid(SpanMapRecordV1, [u8; 32], u64),
    /// A complete row that does not check: its stored digest and its bytes.
    Invalid([u8; 32], u64),
    /// A complete row whose length is over the row bound.
    Oversized,
    /// Fewer bytes than a row needs.
    Short,
}

fn read_row(buf: &[u8], at: usize, prev: &[u8; 32]) -> RowRead {
    let Some(lenb) = buf.get(at..at + 4) else {
        return RowRead::Short;
    };
    let n = u32::from_le_bytes(lenb.try_into().expect("4 bytes")) as usize;
    let Some(row) = at
        .checked_add(4 + 32)
        .and_then(|e| e.checked_add(n))
        .and_then(|e| buf.get(at..e))
    else {
        return RowRead::Short;
    };
    if n > MAX_ROW_BYTES {
        return RowRead::Oversized;
    }
    let body = &row[4..4 + n];
    let stored: [u8; 32] = row[4 + n..].try_into().expect("32 bytes");
    let total = (4 + n + 32) as u64;
    if row_digest(prev, body) != stored {
        return RowRead::Invalid(stored, total);
    }
    match postcard::from_bytes::<SpanMapRecordV1>(body) {
        Ok(r) if postcard::to_allocvec(&r).ok().as_deref() == Some(body) => {
            RowRead::Valid(r, stored, total)
        }
        _ => RowRead::Invalid(stored, total),
    }
}

/// True if any complete row that chains from the stored digest of its predecessor starts in
/// `buf[at..]`, following the length prefixes of the rows from `at`: an invalid row followed by
/// a valid one is damage, not a torn tail.
fn valid_row_follows(buf: &[u8], mut at: usize, mut prev: [u8; 32]) -> bool {
    while at < buf.len() {
        match read_row(buf, at, &prev) {
            RowRead::Valid(..) => return true,
            RowRead::Invalid(stored, n) => {
                prev = stored;
                at += n as usize;
            }
            RowRead::Short | RowRead::Oversized => return false,
        }
    }
    false
}

/// The value of `x` rounded up to a multiple of `a`.
pub(crate) fn round_up(x: u64, a: u64) -> Option<u64> {
    x.checked_add(a - 1).map(|v| v / a * a)
}

/// The value of `x` rounded down to a multiple of `a`.
pub(crate) fn round_down(x: u64, a: u64) -> u64 {
    x / a * a
}

/// Whether `[s, e)` overlaps any range of `ranges`.
fn overlaps(ranges: &[(u64, u64)], s: u64, e: u64) -> bool {
    ranges.iter().any(|(a, b)| s < *b && *a < e)
}

impl SpanMap {
    /// An empty map with no file: a log no store directory holds.
    pub fn empty() -> SpanMap {
        SpanMap::default()
    }

    /// The path of the span map of the store at `store_dir`.
    pub fn path_in(store_dir: &Path) -> PathBuf {
        store_dir.join(SPANMAP_FILE)
    }

    /// Creates the empty span map of a new store: its header, synced, and its directory synced.
    pub fn create(store_dir: &Path) -> Result<(), StoreError> {
        let path = SpanMap::path_in(store_dir);
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut header = [0u8; FILE_HEADER as usize];
        header[..8].copy_from_slice(&MAGIC);
        header[8..].copy_from_slice(&VERSION.to_le_bytes());
        pio::write_all_at(&f, &header, 0)?;
        f.sync_all()?;
        pio::sync_dir(store_dir)?;
        Ok(())
    }

    /// Loads the span map of the store at `store_dir`. A missing file is an empty map. Only the
    /// header, the digest chain and each row's own bounds are checked here; no frame is read.
    /// A short row or a row that does not check at the end of the journal is a torn tail: a
    /// writable open copies it to `SPANMAP.torn-{offset}` and cuts it, a read-only open ignores
    /// it. A row that does not check followed by a valid one, a damaged header, a forward
    /// source, an overlap or a non-monotonic Through is damage.
    pub fn load(store_dir: &Path, mode: MapOpen) -> Result<SpanMap, StoreError> {
        let path = SpanMap::path_in(store_dir);
        let mut map = SpanMap {
            writable: mode == MapOpen::Writable,
            ..SpanMap::default()
        };
        let f = match OpenOptions::new()
            .read(true)
            .write(mode == MapOpen::Writable)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(map),
            Err(e) => return Err(e.into()),
        };
        let file_len = f.metadata()?.len();
        if file_len < FILE_HEADER {
            return Err(corrupt());
        }
        let scan_len = file_len.min(MAX_JOURNAL_BYTES);
        let mut buf = Vec::new();
        buf.try_reserve_exact(scan_len as usize)
            .map_err(|_| StoreError::DedupLimit)?;
        buf.resize(scan_len as usize, 0);
        pio::read_exact_at(&f, &mut buf, 0)?;
        if buf[..8] != MAGIC || buf[8..12] != VERSION.to_le_bytes() {
            return Err(corrupt());
        }
        let mut at = FILE_HEADER as usize;
        let mut prev = [0u8; 32];
        while at < buf.len() {
            match read_row(&buf, at, &prev) {
                RowRead::Valid(record, digest, n) => {
                    if map.rows >= MAX_ROWS {
                        return Err(corrupt());
                    }
                    map.admit(record)?;
                    map.rows += 1;
                    prev = digest;
                    at += n as usize;
                }
                RowRead::Invalid(stored, n) => {
                    if valid_row_follows(&buf, at + n as usize, stored) {
                        return Err(corrupt());
                    }
                    break;
                }
                RowRead::Oversized => return Err(StoreError::DedupLimit),
                RowRead::Short => break,
            }
        }
        map.digest = prev;
        map.len = at as u64;
        if (at as u64) < file_len {
            match mode {
                MapOpen::ReadOnly => map.torn = true,
                MapOpen::Writable => {
                    let q = quarantine_tail(&f, store_dir, at as u64, file_len)?;
                    f.set_len(at as u64)?;
                    f.sync_all()?;
                    pio::sync_dir(store_dir)?;
                    map.quarantined = Some(q);
                }
            }
        }
        if mode == MapOpen::Writable {
            *map.file.get_mut().unwrap_or_else(|e| e.into_inner()) = Some(f);
        }
        Ok(map)
    }

    /// Adds one checked row to the in-memory map.
    fn admit(&mut self, record: SpanMapRecordV1) -> Result<(), StoreError> {
        match record {
            SpanMapRecordV1::Through { end } => {
                if self.through.is_some_and(|t| t > end) {
                    return Err(corrupt());
                }
                self.through = Some(end);
                Ok(())
            }
            SpanMapRecordV1::Map { target, source } => {
                self.check_mapping(&target, &source)?;
                self.insert(Mapping { target, source });
                Ok(())
            }
        }
    }

    /// The metadata checks of one mapping against the map so far: both ranges in bounds, equal
    /// lengths, the source frame ending before the target frame starts, no overlap with another
    /// target range or a protected source range, and a source that is no mapped range.
    fn check_mapping(&self, target: &FrameSpan, source: &FrameSpan) -> Result<(), StoreError> {
        if !target.in_bounds() || !source.in_bounds() || target.len != source.len {
            return Err(corrupt());
        }
        if source.frame_end() > target.place {
            return Err(corrupt());
        }
        let (ts, te) = target.range();
        let (ss, se) = source.range();
        if let Some(ms) = self.targets.get(&target.place) {
            if ms.iter().any(|m| {
                (m.target.raw_kind, m.target.payload_len, m.target.hash)
                    != (target.raw_kind, target.payload_len, target.hash)
            }) {
                return Err(corrupt());
            }
            let ranges: Vec<(u64, u64)> = ms.iter().map(|m| m.target.range()).collect();
            if overlaps(&ranges, ts, te) {
                return Err(corrupt());
            }
        }
        if let Some(prot) = self.sources.get(&target.place)
            && overlaps(prot, ts, te)
        {
            return Err(corrupt());
        }
        if let Some(ms) = self.targets.get(&source.place) {
            let ranges: Vec<(u64, u64)> = ms.iter().map(|m| m.target.range()).collect();
            if overlaps(&ranges, ss, se) {
                return Err(corrupt());
            }
        }
        Ok(())
    }

    fn insert(&mut self, m: Mapping) {
        let v = self.targets.entry(m.target.place).or_default();
        let at = v.partition_point(|x| x.target.start < m.target.start);
        v.insert(at, m);
        let s = self.sources.entry(m.source.place).or_default();
        let r = m.source.range();
        let at = s.partition_point(|x| *x < r);
        s.insert(at, r);
    }

    /// Checks the loaded map against the sealed log's end `end`: no row names a frame or a
    /// Through past it.
    pub fn check_sealed(&self, end: Location) -> Result<(), StoreError> {
        if self.through.is_some_and(|t| t > end) {
            return Err(corrupt());
        }
        for ms in self.targets.values() {
            for m in ms {
                if m.target.frame_end() > end || m.source.frame_end() > end {
                    return Err(corrupt());
                }
            }
        }
        Ok(())
    }

    /// Fills the mapped ranges of the frame at `place` (raw kind `raw_kind`, hash `hash`) in
    /// `payload` from their sources, read through `files`. A range whose target header facts
    /// or whose source header does not match is left as read: the frame's own hash check then
    /// refuses it. A frame with no mapping is left as it is.
    pub(crate) fn overlay(
        &self,
        files: &dyn SegmentFiles,
        place: Location,
        raw_kind: u8,
        hash: Hash,
        payload: &mut [u8],
    ) -> io::Result<()> {
        let len = payload.len() as u64;
        self.overlay_window(files, place, raw_kind, hash, len, 0, payload)
    }

    /// [`SpanMap::overlay`] of the window of the payload (of `payload_len` bytes) that starts
    /// at payload offset `at` and is `buf.len()` bytes long.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn overlay_window(
        &self,
        files: &dyn SegmentFiles,
        place: Location,
        raw_kind: u8,
        hash: Hash,
        payload_len: u64,
        at: u64,
        buf: &mut [u8],
    ) -> io::Result<()> {
        let Some(ms) = self.targets.get(&place) else {
            return Ok(());
        };
        let (ws, we) = (at, at + buf.len() as u64);
        for m in ms {
            let t = &m.target;
            if (t.raw_kind, t.hash, u64::from(t.payload_len)) != (raw_kind, hash, payload_len) {
                continue;
            }
            let (ts, te) = t.range();
            let (s, e) = (ts.max(ws), te.min(we));
            if s >= e {
                continue;
            }
            let src = &m.source;
            if files.header(src.place)? != Some((src.payload_len, src.raw_kind, src.hash)) {
                continue;
            }
            files.read_at(
                src.place.segment,
                &mut buf[(s - ws) as usize..(e - ws) as usize],
                src.physical_start() + (s - ts),
            )?;
        }
        Ok(())
    }

    /// The live mapped bytes: the target bytes of every mapping whose frame `removed` does not
    /// name.
    pub fn mapped_bytes(&self, removed: impl Fn(&Location) -> bool) -> u64 {
        self.mappings()
            .filter(|m| !removed(&m.target.place))
            .map(|m| u64::from(m.target.len))
            .sum()
    }

    /// Whether the frame at `place` has a mapped range.
    pub fn is_target(&self, place: &Location) -> bool {
        self.targets.contains_key(place)
    }

    /// The mappings of the frame at `place`.
    pub fn mappings_of(&self, place: &Location) -> &[Mapping] {
        self.targets.get(place).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every mapping, by target place.
    pub fn mappings(&self) -> impl Iterator<Item = &Mapping> {
        self.targets.values().flatten()
    }

    /// The target places whose mapping reads from the frame at `source`.
    pub fn dependents_of(&self, source: &Location) -> Vec<Location> {
        if !self.sources.contains_key(source) {
            return vec![];
        }
        let mut out: Vec<Location> = self
            .mappings()
            .filter(|m| m.source.place == *source)
            .map(|m| m.target.place)
            .collect();
        out.dedup();
        out
    }

    /// The end of the last durable Through row.
    pub fn through(&self) -> Option<Location> {
        self.through
    }

    /// The durable rows.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// The valid journal bytes.
    pub fn journal_len(&self) -> u64 {
        self.len
    }

    /// Whether a read-only open ignored a torn suffix.
    pub fn torn(&self) -> bool {
        self.torn
    }

    /// The quarantine file a writable open wrote, if any.
    pub fn quarantined(&self) -> Option<&Path> {
        self.quarantined.as_deref()
    }

    /// Whether a pass found the map full.
    pub fn is_full(&self) -> bool {
        self.full
    }

    pub(crate) fn set_full(&mut self) {
        self.full = true;
    }

    /// Why threshold passes stopped on this handle.
    pub fn stopped(&self) -> Option<StopReason> {
        self.stopped
    }

    pub(crate) fn stop(&mut self, why: StopReason) {
        self.stopped.get_or_insert(why);
    }

    /// Whether a threshold pass may run on this handle.
    pub fn threshold_allowed(&self) -> bool {
        self.writable && self.stopped.is_none() && !self.threshold_off
    }

    pub(crate) fn disable_threshold(&mut self) {
        self.threshold_off = true;
    }

    /// The notes a compact or status reports for this map's state.
    pub fn notes(&self) -> Vec<String> {
        let mut out = vec![];
        if let Some(why) = self.stopped {
            out.push(why.note());
        }
        if self.full {
            out.push(FULL_NOTE.to_string());
        }
        out
    }

    /// Whether `rows` more rows of at most the largest size fit in the journal.
    pub(crate) fn has_room(&self, rows: u64, bytes: u64) -> bool {
        self.rows.saturating_add(rows) <= MAX_ROWS
            && self.len.max(FILE_HEADER).saturating_add(bytes) <= MAX_JOURNAL_BYTES
    }

    /// Whether a staged mapping may be added beside the map and `staged`: its target overlaps
    /// no mapped or protected range, and its source no mapped range.
    pub(crate) fn admits(&self, staged: &[Mapping], m: &Mapping) -> bool {
        let mut probe = SpanMap {
            targets: BTreeMap::new(),
            sources: BTreeMap::new(),
            ..SpanMap::default()
        };
        for x in self
            .targets
            .get(&m.target.place)
            .into_iter()
            .flatten()
            .chain(self.targets.get(&m.source.place).into_iter().flatten())
            .chain(staged.iter())
        {
            probe.insert(*x);
        }
        if let Some(p) = self.sources.get(&m.target.place) {
            probe
                .sources
                .entry(m.target.place)
                .or_default()
                .extend_from_slice(p);
        }
        probe.check_mapping(&m.target, &m.source).is_ok()
    }

    /// Appends `maps` and, when given, one `Through { end }` row, then syncs the file once (a
    /// new file is created with its header, and its directory synced once). Nothing is
    /// published: [`SpanMap::publish`] does that once this returns. A failed append or sync
    /// leaves the in-memory map as it was; its bytes are a torn tail the next writable open
    /// quarantines.
    pub(crate) fn append(
        &self,
        store_dir: &Path,
        maps: &[Mapping],
        through: Option<Location>,
    ) -> Result<Staged, StoreError> {
        let mut records: Vec<SpanMapRecordV1> = Vec::new();
        records
            .try_reserve_exact(maps.len() + 1)
            .map_err(|_| StoreError::DedupLimit)?;
        records.extend(maps.iter().map(|m| SpanMapRecordV1::Map {
            target: m.target,
            source: m.source,
        }));
        if let Some(end) = through {
            records.push(SpanMapRecordV1::Through { end });
        }
        if records.is_empty() {
            return Ok(Staged::default());
        }
        let mut bytes = Vec::new();
        let mut digest = self.digest;
        for r in &records {
            let (row, d) = encode_row(&digest, r)?;
            bytes
                .try_reserve(row.len())
                .map_err(|_| StoreError::DedupLimit)?;
            bytes.extend_from_slice(&row);
            digest = d;
        }
        if self.rows + records.len() as u64 > MAX_ROWS
            || self.len.max(FILE_HEADER) + bytes.len() as u64 > MAX_JOURNAL_BYTES
        {
            return Err(StoreError::DedupLimit);
        }
        event(MapEvent::Append {
            maps: maps.len(),
            through: through.is_some(),
        })?;
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut start = self.len;
        if guard.is_none() {
            if !self.writable {
                return Err(StoreError::Io(io::ErrorKind::PermissionDenied.into()));
            }
            let path = SpanMap::path_in(store_dir);
            let f = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    SpanMap::create(store_dir)?;
                    OpenOptions::new().read(true).write(true).open(&path)?
                }
                Err(e) => return Err(e.into()),
            };
            *guard = Some(f);
            start = start.max(FILE_HEADER);
        }
        let f = guard.as_ref().expect("an open span map file");
        let start = start.max(FILE_HEADER);
        pio::write_all_at(f, &bytes, start)?;
        event(MapEvent::Sync)?;
        f.sync_all()?;
        fsutil::record(&SpanMap::path_in(store_dir));
        Ok(Staged {
            records,
            digest,
            len: start + bytes.len() as u64,
        })
    }

    /// Publishes rows that [`SpanMap::append`] made durable.
    pub(crate) fn publish(&mut self, staged: Staged) -> Result<(), StoreError> {
        if staged.records.is_empty() {
            return Ok(());
        }
        let _ = event(MapEvent::Publish);
        for r in staged.records {
            self.admit(r)?;
            self.rows += 1;
        }
        self.digest = staged.digest;
        self.len = staged.len;
        Ok(())
    }
}

/// Rows a pass made durable, not yet published.
#[derive(Default)]
pub(crate) struct Staged {
    records: Vec<SpanMapRecordV1>,
    digest: [u8; 32],
    len: u64,
}

/// Copies `[offset, file_len)` of `f` to `SPANMAP.torn-{offset}` (or the first unused
/// `SPANMAP.torn-{offset}.{n}`), syncs it and the directory, and returns its path.
fn quarantine_tail(
    f: &File,
    store_dir: &Path,
    offset: u64,
    file_len: u64,
) -> Result<PathBuf, StoreError> {
    let base = format!("{SPANMAP_FILE}.torn-{offset}");
    let mut n = 0u32;
    let (q, path) = loop {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}.{n}")
        };
        let path = store_dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(q) => break (q, path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                n += 1;
                if n > MAX_QUARANTINE_NAMES {
                    return Err(e.into());
                }
            }
            Err(e) => return Err(e.into()),
        }
    };
    let mut buf = vec![0u8; 65_536];
    let mut at = offset;
    while at < file_len {
        let k = (file_len - at).min(buf.len() as u64) as usize;
        pio::read_exact_at(f, &mut buf[..k], at)?;
        pio::write_all_at(&q, &buf[..k], at - offset)?;
        at += k as u64;
    }
    q.sync_all()?;
    pio::sync_dir(store_dir)?;
    Ok(path)
}

type Registry = Vec<(crate::fileid::FileId, Weak<RwLock<SpanMap>>)>;

/// The span maps of the logs open in this process, by the identity of their first segment
/// file: the redaction paths that read a log through its raw segment files find the map of
/// that log here, so they read mapped payloads as every other read does.
static REGISTRY: Mutex<Registry> = Mutex::new(Vec::new());

/// Registers `spans` as the span map of the log whose segment files are `readers`.
pub(crate) fn register(readers: &[File], spans: &Arc<RwLock<SpanMap>>) {
    let Some(first) = readers.first() else {
        return;
    };
    let Ok(id) = crate::fileid::FileId::of_file(first) else {
        return;
    };
    let mut r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|(_, w)| w.strong_count() > 0);
    r.push((id, Arc::downgrade(spans)));
}

/// The span map of the log whose segment files are `readers`, if one is open: of several, the
/// one with the most rows.
pub(crate) fn spans_for(readers: &[File]) -> Option<Arc<RwLock<SpanMap>>> {
    let first = readers.first()?;
    {
        let r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        if r.is_empty() {
            return None;
        }
    }
    let id = crate::fileid::FileId::of_file(first).ok()?;
    let r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    r.iter()
        .filter(|(k, _)| *k == id)
        .filter_map(|(_, w)| w.upgrade())
        .max_by_key(|m| m.read().map(|m| m.rows()).unwrap_or(0))
}

/// Fills the mapped ranges of the frame at `loc` of the log `readers` in `payload`, read from
/// its raw segment file: the frame's own header gives its kind and hash. A log with no open
/// span map is left as it is.
pub(crate) fn overlay_raw(readers: &[File], loc: Location, payload: &mut [u8]) -> io::Result<()> {
    let Some(spans) = spans_for(readers) else {
        return Ok(());
    };
    let map = spans.read().unwrap_or_else(|e| e.into_inner());
    if !map.is_target(&loc) {
        return Ok(());
    }
    let Some((len, raw, hash)) = readers.header(loc)? else {
        return Ok(());
    };
    if len as usize != payload.len() {
        return Ok(());
    }
    map.overlay(&crate::segment::LogFiles(readers), loc, raw, hash, payload)
}

/// Whether `name` is a span map quarantine file's name (`SPANMAP.torn-*`).
pub fn is_quarantine_name(name: &str) -> bool {
    name.starts_with("SPANMAP.torn-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(seg: u32, off: u64, len: u32, start: u32, n: u32) -> FrameSpan {
        FrameSpan {
            place: Location {
                segment: seg,
                offset: off,
            },
            raw_kind: 3,
            payload_len: len,
            hash: Hash([7; 32]),
            start,
            len: n,
        }
    }

    #[test]
    fn r5902_rows_fit_their_bound() {
        let m = SpanMapRecordV1::Map {
            target: span(u32::MAX, u64::MAX / 2, u32::MAX, u32::MAX, u32::MAX),
            source: span(u32::MAX, u64::MAX / 2, u32::MAX, u32::MAX, u32::MAX),
        };
        let (row, _) = encode_row(&[0; 32], &m).unwrap();
        assert!(row.len() <= MAX_ROW_BYTES + ROW_OVERHEAD as usize);
        assert_eq!(
            FILE_HEADER + MAX_ROWS * (MAX_ROW_BYTES as u64 + ROW_OVERHEAD),
            MAX_JOURNAL_BYTES
        );
    }

    #[test]
    fn r5902_step_ids_are_feature_names_or_versioned() {
        use crate::format::valid_step_id;
        for ok in ["snapshot_delta", "dedup-spans-1", "a-1", "a_b-c-22"] {
            assert!(valid_step_id(ok), "{ok}");
        }
        for bad in ["", "a-b", "-1", "a--1", "a-", "A-1", "a-1x", "1-1"] {
            assert!(!valid_step_id(bad), "{bad}");
        }
        assert!(!valid_step_id(&format!("{}-1", "a".repeat(63))));
    }
}
