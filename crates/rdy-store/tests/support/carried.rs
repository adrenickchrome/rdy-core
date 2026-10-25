//! The shared carried-record fixtures of the store integration tests. A test binary includes
//! this file with `#[path = "support/carried.rs"] mod carried_fixture;`.
//!
//! - [`load`] is the checked path, a stopped load's ingest: it copies a closed store's sealed
//!   batches into a new store directory, writes each `held: false` span as a dropped frame (its
//!   original header and zeros) that names its carried record, then appends the carried batch
//!   with `Ingest::append_carried`.
//! - [`append_raw`], [`zero`] and [`poke`] are the corrupt-log helpers: they write raw log bytes
//!   of a closed store through the segment log or the segment files, never through a store
//!   writer.
#![allow(dead_code)]

use rdy_model::redaction::{
    BlockResidue, CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget,
    RedactionCarried, RedactionResidue,
};
use rdy_model::{Block, Change, Hash, Provenance, Role};
use rdy_store::dump::{DumpBatch, Frame};
use rdy_store::error::StoreError;
use rdy_store::format::{DumpStore, baseline_features};
use rdy_store::ingest::{Ingest, IngestReport};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::{Store, StoreReader};
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

/// The segment size the store opens with.
const SEGMENT_MAX: u64 = 64 * 1024 * 1024;

/// The bytes of a frame header: length, kind and hash.
pub const HEADER: u64 = 4 + 1 + 32;

/// This rdy's store format.
pub fn store_format() -> DumpStore {
    DumpStore::Features(baseline_features())
}

/// The source request, apply and landing a carried record names; none of them is in any test
/// store.
pub fn source_request() -> Hash {
    Hash::of(b"source request")
}
pub fn source_apply() -> Hash {
    Hash::of(b"source apply")
}
pub fn source_landing() -> Hash {
    Hash::of(b"source landing")
}

/// The person who approved the source removal, at the source's own time.
pub fn approver() -> Provenance {
    Provenance {
        actor: "human:alice".into(),
        run: Some("source-run".into()),
        role: Role::Approver,
        at_ms: 777,
    }
}

/// One frame of a log: where it is, its kind, its hash and its payload length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framed {
    pub loc: Location,
    pub kind: u8,
    pub hash: Hash,
    pub len: u32,
}

impl Framed {
    /// The frame's end.
    pub fn end(&self) -> Location {
        Location {
            segment: self.loc.segment,
            offset: self.loc.offset + HEADER + u64::from(self.len),
        }
    }
}

/// Every data frame of the log `reader` reads, up to `end`, in log order.
pub fn framed(reader: &StoreReader, end: Location) -> Vec<Framed> {
    let mut out = vec![];
    reader
        .for_each_record_from(
            Location {
                segment: 0,
                offset: 0,
            },
            end,
            |loc, f, _| {
                let (kind, hash, len) = match f {
                    StoredFrame::Present {
                        kind,
                        hash,
                        payload,
                    } => (kind, hash, payload.len() as u32),
                    StoredFrame::Dropped {
                        kind, hash, len, ..
                    } => (kind, hash, len),
                };
                if kind != kinds::BATCH_COMMIT {
                    out.push(Framed {
                        loc,
                        kind,
                        hash,
                        len,
                    });
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

/// The frames of the closed store at `dir`.
pub fn framed_at(dir: &Path) -> Vec<Framed> {
    let s = Store::open(dir).unwrap();
    let r = s.reader();
    framed(&r, s.log_end())
}

/// The first frame of `kind` and `hash` in `frames`.
pub fn find(frames: &[Framed], kind: u8, hash: Hash) -> Framed {
    *frames
        .iter()
        .find(|f| f.kind == kind && f.hash == hash)
        .unwrap_or_else(|| panic!("a kind {kind} frame {hash}"))
}

/// The residue of block `b`.
pub fn block_residue(b: &Block) -> RedactionResidue {
    RedactionResidue::Block(BlockResidue::of(b))
}

/// The residue of change `c`.
pub fn change_residue(c: &Change) -> RedactionResidue {
    RedactionResidue::Change(ChangeResidue::of(c))
}

/// The frame of a new residue `r` as the store writes one, in its kind's latest encoding.
pub fn residue_frame(r: &RedactionResidue) -> (u8, Vec<u8>) {
    rdy_store::codec::frame_latest(kinds::REDACTION_RESIDUE, postcard::to_allocvec(r).unwrap())
}

/// The hash of a new residue `r`: its frame's.
pub fn residue_hash(r: &RedactionResidue) -> Hash {
    Hash::of(&residue_frame(r).1)
}

/// The carried span of frame `f`, with `residue` and `held`.
pub fn span(f: &Framed, residue: Option<&RedactionResidue>, held: bool) -> CarriedSpan {
    CarriedSpan {
        place: FramePlace {
            segment: f.loc.segment,
            offset: f.loc.offset,
        },
        kind: f.kind,
        hash: f.hash,
        len: f.len,
        residue: residue.map(residue_hash),
        held,
    }
}

/// A carried record of the source removal (`source_request`, `source_apply`,
/// `source_landing`, approved by `approver`) for `targets`, over `spans`.
pub fn record(targets: Vec<RedactTarget>, spans: Vec<CarriedSpan>) -> RedactionCarried {
    RedactionCarried {
        request: source_request(),
        apply: source_apply(),
        landing: source_landing(),
        approved_by: approver(),
        reason_class: ReasonClass::Secret,
        targets,
        spans,
    }
}

/// The hash of a carried record's encoding.
pub fn carried_hash(r: &RedactionCarried) -> Hash {
    Hash::of(&postcard::to_allocvec(r).unwrap())
}

/// The sealed batches of the closed store at `src`, with each `held: false` span of `records`
/// as a dropped frame that names its first record.
pub fn batches_with_drops(src: &Path, records: &[RedactionCarried]) -> Vec<DumpBatch> {
    let mut drops: BTreeMap<Location, (Hash, Option<Hash>)> = BTreeMap::new();
    for r in records {
        let h = carried_hash(r);
        for s in r.spans.iter().filter(|s| !s.held) {
            drops
                .entry(Location {
                    segment: s.place.segment,
                    offset: s.place.offset,
                })
                .or_insert((h, s.residue));
        }
    }
    let s = Store::open(src).unwrap();
    let reader = s.reader();
    let mut out = vec![];
    reader
        .for_each_batch(s.log_end(), |mut b| {
            let mut at = b.start;
            for f in b.frames.iter_mut() {
                let len = f.original_len();
                if let Some((apply, residue)) = drops.get(&at) {
                    *f = Frame::Dropped {
                        kind: f.kind(),
                        hash: *f.hash(),
                        len,
                        apply: *apply,
                        residue: *residue,
                    };
                }
                at.offset += HEADER + u64::from(len);
            }
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// The checked path: ingests `batches` into the new store directory `to`, appends the carried
/// batch of `residues` and `records`, and finishes the store.
pub fn ingest(
    to: &Path,
    batches: &[DumpBatch],
    residues: &[RedactionResidue],
    records: &[RedactionCarried],
) -> Result<IngestReport, StoreError> {
    let mut i = Ingest::create(to)?;
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b)?;
    }
    i.append_carried(residues, records)?;
    i.upgrade(&store_format())?;
    i.finish()
}

/// [`ingest`] of the closed store at `src` into `to`, its `held: false` spans dropped.
pub fn load(
    src: &Path,
    to: &Path,
    residues: &[RedactionResidue],
    records: &[RedactionCarried],
) -> Result<IngestReport, StoreError> {
    ingest(to, &batches_with_drops(src, records), residues, records)
}

/// Appends `frames` to the closed store at `dir` as one batch, with its marker when `seal`.
/// Returns where each frame went.
pub fn append_raw(dir: &Path, frames: &[(u8, Vec<u8>)], seal: bool) -> Vec<Location> {
    let (mut log, quarantined) =
        SegmentLog::open_marked(&dir.join("segments"), SEGMENT_MAX, None).unwrap();
    assert!(quarantined.is_empty(), "the store has a torn tail");
    let placed = if seal {
        log.append_batch(frames).unwrap()
    } else {
        log.append_frames(frames, true).unwrap()
    };
    log.sync().unwrap();
    placed.into_iter().map(|(l, _)| l).collect()
}

/// The residue and carried frames of a carried batch: each residue once, by residue hash, then
/// each record in order.
pub fn carried_frames(
    residues: &[RedactionResidue],
    records: &[RedactionCarried],
) -> Vec<(u8, Vec<u8>)> {
    let mut by_hash: BTreeMap<Hash, (u8, Vec<u8>)> = BTreeMap::new();
    for r in residues {
        let f = residue_frame(r);
        by_hash.insert(Hash::of(&f.1), f);
    }
    let mut out: Vec<(u8, Vec<u8>)> = by_hash.into_values().collect();
    out.extend(
        records
            .iter()
            .map(|r| (kinds::REDACTION_CARRIED, postcard::to_allocvec(r).unwrap())),
    );
    out
}

fn segment_file(dir: &Path, segment: u32) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("segments").join(format!("seg-{segment:06}.log")))
        .unwrap()
}

/// Overwrites the payload of frame `f` of the closed store at `dir` with zeros.
pub fn zero(dir: &Path, f: &Framed) {
    let mut file = segment_file(dir, f.loc.segment);
    file.seek(SeekFrom::Start(f.loc.offset + HEADER)).unwrap();
    file.write_all(&vec![0u8; f.len as usize]).unwrap();
    file.sync_all().unwrap();
}

/// Writes `byte` at payload byte `at` of frame `f` of the closed store at `dir`.
pub fn poke(dir: &Path, f: &Framed, at: u32, byte: u8) {
    let mut file = segment_file(dir, f.loc.segment);
    file.seek(SeekFrom::Start(f.loc.offset + HEADER + u64::from(at)))
        .unwrap();
    file.write_all(&[byte]).unwrap();
    file.sync_all().unwrap();
}

/// The bytes of every segment file of the store at `dir`, by file name.
pub fn log_bytes(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join("segments")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(entry.path()).unwrap());
        }
    }
    out
}

/// Copies the store directory `from` to `to` (which must not exist), file by file.
pub fn copy_store(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.join("segments")).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.is_file() {
            std::fs::copy(&p, to.join(entry.file_name())).unwrap();
        }
    }
    for entry in std::fs::read_dir(from.join("segments")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join("segments").join(entry.file_name())).unwrap();
    }
}
