//! The log ingest of a load (plan Dump and load, D-WWAK7, D-D2NPN): it writes a dump's sealed
//! batches into a new store directory, each at the place the source log held it. So the new log
//! is the source log byte for byte, and every location inside a payload stays true: a ref's salt
//! (`RefUpdate.at`), an inbox cursor (`InboxCursor.at`) and a marker's start
//! (`BatchCommit.start`). The ingest writes no index: the first open of the store builds it from
//! the log.
//!
//! The new log never rolls by itself. It rolls only where the source rolled: before a batch that
//! starts at offset 0 of the next segment. So a held batch that passed the soft segment limit in
//! the source passes it here too.
//!
//! A converter for another store format must remap every location inside a payload
//! (`RefUpdate.at`, `InboxCursor.at`) if it changes the layout.
use crate::dump::DumpBatch;
use crate::error::StoreError;
use crate::format::{self, Format};
use crate::fsutil::sync_dir;
use crate::kinds::batch_digest;
use crate::segment::{Location, SegmentLog};
use std::path::{Path, PathBuf};

/// The directory of the log inside a store directory.
const SEGMENTS_DIR: &str = "segments";

/// A new store directory that takes a dump's batches in log order.
pub struct Ingest {
    dir: PathBuf,
    log: SegmentLog,
    batches: u64,
    frames: u64,
}

/// What an ingest wrote: batches, data frames, and the end of the new log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngestReport {
    pub batches: u64,
    pub frames: u64,
    pub end: Location,
}

fn dump_error(what: String) -> StoreError {
    StoreError::Dump { at: 0, what }
}

impl Ingest {
    /// Creates the store directory `dir` with its `FORMAT` (this rdy's store format) and an empty
    /// log. `dir` must not exist: an existing `dir` is `StoreError::Io` of kind `AlreadyExists`,
    /// and nothing is written into it.
    pub fn create(dir: &Path) -> Result<Ingest, StoreError> {
        std::fs::create_dir(dir)?;
        let parent = match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        sync_dir(parent)?;
        format::write_format_file(dir, Format::CURRENT)?;
        let segments = dir.join(SEGMENTS_DIR);
        std::fs::create_dir(&segments)?;
        sync_dir(dir)?;
        // the log never rolls by itself: `batch` rolls where the source rolled
        let log = SegmentLog::open(&segments, u64::MAX)?;
        Ok(Ingest {
            dir: dir.to_path_buf(),
            log,
            batches: 0,
            frames: 0,
        })
    }

    /// Appends batch `n` of the dump at its source place, then its marker. When the batch starts
    /// in the next segment and the current segment holds frames, the log rolls once first. The
    /// log's end must then equal the batch's start. The marker's digest must equal the batch's
    /// digest. Either failure is `StoreError::Dump`. Nothing is synced until `finish`.
    pub fn batch(&mut self, n: u64, b: &DumpBatch) -> Result<(), StoreError> {
        let end = self.log.end();
        if b.start.segment == end.segment.wrapping_add(1) && end.offset > 0 {
            self.log.roll()?;
        }
        let end = self.log.end();
        if end != b.start {
            return Err(dump_error(format!(
                "batch {n} starts at segment {} offset {}, and the log ends at segment {} offset {}",
                b.start.segment, b.start.offset, end.segment, end.offset
            )));
        }
        if b.frames.is_empty() {
            return Err(dump_error(format!("batch {n} has no frames")));
        }
        let frames: Vec<(u8, Vec<u8>)> = b
            .frames
            .iter()
            .map(|f| (f.kind, f.payload.clone()))
            .collect();
        let placed = self.log.append_frames(&frames, false)?;
        let hashes: Vec<_> = placed.iter().map(|(_, h)| *h).collect();
        if batch_digest(&hashes) != b.digest {
            return Err(dump_error(format!("batch {n} does not match its digest")));
        }
        self.log.append_marker(b.start, hashes.iter())?;
        self.batches += 1;
        self.frames += placed.len() as u64;
        Ok(())
    }

    /// Makes the new log durable: one sync of the log, then of `segments/` and of the store
    /// directory. Writes no index.
    pub fn finish(mut self) -> Result<IngestReport, StoreError> {
        self.log.sync()?;
        sync_dir(&self.dir.join(SEGMENTS_DIR))?;
        sync_dir(&self.dir)?;
        Ok(IngestReport {
            batches: self.batches,
            frames: self.frames,
            end: self.log.end(),
        })
    }
}
