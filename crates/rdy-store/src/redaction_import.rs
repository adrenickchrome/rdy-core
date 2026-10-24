//! A holder's removal authorization (D-3M81W: rdy "drops their bytes and keeps their hashes";
//! D-CC90E: a removal needs a person's approval; D-BPYN8: a holder; O2: the logical tuple rule).
//!
//! A holder takes its applies from other stores. Each request's manifest names the places its
//! spans had in the source log, which mean nothing here. So a holder never reads a span's place:
//!
//! 1. Each sealed apply is checked by object links alone (`sync_proof::check`, over the
//!    holder's own log): its human landing names the request, the carrier and the approved
//!    proposal agree, and the request's spans give its approved logical tuples
//!    `(kind, hash, len, residue)`. All-versions targets add nothing at the receiver.
//! 2. Every frame of the sealed log whose kind and hash is an approved tuple is a span: its
//!    header must keep the tuple's length, and an intact payload must be what the residue
//!    records (`residue_matches`). An all-zero or partly zero payload is an erased or
//!    interrupted copy. The earliest apply in log order that approves a key authorizes its
//!    frames, as an earlier apply keeps a frame in a project. Every apply that approves a key
//!    must give it the same tuple: another length or residue is damage at the later apply.
//! 3. An intact CHANGE outside the approved tuples that embeds an approved block version is
//!    damage: no approval covers that copy of the removed content.
//!
//! Nothing here is stored: no kind 12, no map record, no second approval. The open, a reindex,
//! an ingest of a load and the take derive the same spans from the same sealed log; the index
//! is never authority. Which branch a log takes is fixed by the mode its first sealed batch sets
//! (`crate::holder`), read before this bootstrap runs: a project keeps its native physical
//! validation (`crate::redaction_io`), and no holder can read a native apply's places.
//!
//! The work is bounded by [`ReceiverCaps`]: at most 1,000,000 frames and 2,147,483,648 payload
//! bytes of the log scanned, and at most 1,000,000 derived spans. A log over them fails closed
//! (`RedactionLimit`), and the take refuses a batch that would take its log over them. These are
//! work caps, not a retained map: the key locations a scan collects live only while it runs.
use crate::error::StoreError;
use crate::holder::{FirstBatch, StoreMode};
use crate::index::RedactedSpan;
use crate::kinds;
use crate::object_refs::ObjectKey;
use crate::redaction::{limit, residue_matches};
use crate::redaction_io::{
    Applied, Authorization, Collected, FrameLog, LogAccess, SpanAuth, collect, read_record,
};
use crate::segment::{self, HEADER, Hole, Holes, Location, header_at};
use crate::stream_error::StreamError;
use crate::sync_proof::{self, ObjectSource, PortableRemoval};
use crate::sync_stream::StreamRecord;
use rdy_model::redaction::{MAX_RECORD_BYTES, RedactionResidue};
use rdy_model::{Change, Hash, Op};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;

/// The most frames of a receiver log a removal scan reads (inclusive).
pub const MAX_RECEIVER_FRAMES: u64 = 1_000_000;
/// The most payload bytes of a receiver log a removal scan reads (inclusive).
pub const MAX_RECEIVER_BYTES: u64 = 2_147_483_648;
/// The most removable spans a receiver's approvals derive (inclusive).
pub const MAX_RECEIVER_SPANS: u64 = 1_000_000;

/// The work caps of a receiver's removal scan: its frames, its payload bytes and the spans it
/// derives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverCaps {
    pub frames: u64,
    pub bytes: u64,
    pub spans: u64,
}

impl ReceiverCaps {
    /// The caps every holder uses.
    pub const DEFAULT: ReceiverCaps = ReceiverCaps {
        frames: MAX_RECEIVER_FRAMES,
        bytes: MAX_RECEIVER_BYTES,
        spans: MAX_RECEIVER_SPANS,
    };

    /// The caps in force on this thread: [`ReceiverCaps::DEFAULT`] unless a test lowered them.
    pub fn current() -> ReceiverCaps {
        CAPS.with(|c| c.get()).unwrap_or(ReceiverCaps::DEFAULT)
    }

    /// The take's limit label of the frames cap: `1000000 receiver frames`.
    pub fn frames_label(&self) -> String {
        format!("{} receiver frames", self.frames)
    }

    /// The take's limit label of the bytes cap: `2147483648 receiver bytes`.
    pub fn bytes_label(&self) -> String {
        format!("{} receiver bytes", self.bytes)
    }

    /// The take's limit label of the spans cap: `1000000 receiver spans`.
    pub fn spans_label(&self) -> String {
        format!("{} receiver spans", self.spans)
    }
}

thread_local! {
    /// Lowered caps for this thread (test hook); None is the default.
    static CAPS: Cell<Option<ReceiverCaps>> = const { Cell::new(None) };
}

/// Test hook: lowers the receiver caps on this thread (None puts the defaults back), so a test
/// can pin N and N+1 without a log of a million frames. An open and a take on this thread use
/// them.
#[doc(hidden)]
pub fn set_receiver_caps_for_tests(caps: Option<ReceiverCaps>) {
    CAPS.with(|c| c.set(caps));
}

/// What a holder's validated applies approve, derived from its log: each apply's logical
/// removal, and the receiver spans of each request's approved objects (every approval that
/// names a key lists its frames). Not stored.
#[derive(Clone, Debug, Default)]
pub(crate) struct Imported {
    /// Each validated apply's removal, by apply hash.
    pub(crate) removals: BTreeMap<Hash, PortableRemoval>,
    /// The receiver spans of each request that a validated apply approves.
    request_spans: HashMap<Hash, Vec<RedactedSpan>>,
}

impl Imported {
    /// What the validated applies `removals` approve, before any receiver span is derived.
    pub(crate) fn of(removals: BTreeMap<Hash, PortableRemoval>) -> Imported {
        Imported {
            removals,
            request_spans: HashMap::new(),
        }
    }

    /// The receiver spans of request `request` (none when no validated apply approves it).
    pub(crate) fn spans_of(&self, request: &Hash) -> Vec<RedactedSpan> {
        self.request_spans.get(request).cloned().unwrap_or_default()
    }
}

/// The mode the log's first sealed batch sets, read from its frame headers and its ref frames
/// before any redaction bootstrap: the mode decides which bootstrap judges the log. A log with
/// no frames, or whose first batch is torn, is a project's (as `crate::holder::FirstBatch` has
/// it); a mode value other than `holder` is `Decode`.
pub(crate) fn log_mode(readers: &[File]) -> Result<StoreMode, StoreError> {
    let Some(f) = readers.first() else {
        return Ok(StoreMode::Project);
    };
    let file_len = f.metadata()?.len();
    let mut first = FirstBatch::default();
    let mut offset = 0u64;
    while let Some((len, kind, hash)) = header_at(f, offset, file_len)? {
        let loc = Location { segment: 0, offset };
        let next = Location {
            segment: 0,
            offset: offset + HEADER + u64::from(len),
        };
        let payload = if kind == kinds::REF_UPDATE {
            read_record(readers, &BTreeMap::new(), loc, kind, &hash)?
        } else {
            None
        };
        if first.frame(kind, payload.as_deref(), next)? {
            break;
        }
        offset = next.offset;
    }
    Ok(first.creation(Location { segment: 0, offset }).mode)
}

/// The kinds a removal proof reads by key (`sync_proof::check`): an apply, its landing and
/// approved proposal, its request and carrier, residues and blob manifests.
pub(crate) const PROOF_KINDS: [u8; 7] = [
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::BLOB_MANIFEST,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
];

/// A holder log as a proof source: the first frame of each proof kind and hash before the
/// scan's end, served when its payload is intact. Records of other kinds, and erased or damaged
/// ones, are not served.
pub(crate) struct LogSource<'a> {
    log: &'a mut dyn FrameLog,
    first: HashMap<ObjectKey, Location>,
}

impl ObjectSource for LogSource<'_> {
    fn record(&mut self, key: ObjectKey) -> Result<Option<StreamRecord>, StreamError> {
        let Some(loc) = self.first.get(&key).copied() else {
            return Ok(None);
        };
        let io = |e: StoreError| StreamError::io(None, 0, &std::io::Error::other(e.to_string()));
        let header_io = |e: StoreError| match e {
            StoreError::Io(e) => StreamError::io(None, 0, &e),
            e => io(e),
        };
        let raw = match self.log.header_at(loc).map_err(header_io)? {
            Some((len, raw, _)) if len as usize <= MAX_RECORD_BYTES => raw,
            _ => return Ok(None),
        };
        let payload = self.log.record(loc, key.kind, &key.hash).map_err(io)?;
        Ok(payload.and_then(|payload| {
            let encoding = crate::codec::split(raw, &payload).ok()?.version;
            Some(StreamRecord::Present {
                key,
                encoding,
                payload,
            })
        }))
    }
}

/// The removal-scan counts of a log: its frames and their payload bytes, each within its cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scanned {
    pub(crate) frames: u64,
    pub(crate) bytes: u64,
}

impl Scanned {
    /// Counts one frame of `len` payload bytes; over a cap is `RedactionLimit`.
    pub(crate) fn add(&mut self, len: u32, caps: &ReceiverCaps) -> Result<(), StoreError> {
        self.frames += 1;
        self.bytes += u64::from(len);
        if self.frames > caps.frames {
            return Err(StoreError::RedactionLimit {
                limit: limit::FRAMES,
            });
        }
        if self.bytes > caps.bytes {
            return Err(StoreError::RedactionLimit {
                limit: limit::BYTES,
            });
        }
        Ok(())
    }
}

/// One header pass over the log before `upto`: the counts within the caps, and the first
/// location of every proof-kind key.
fn locate(
    log: &mut dyn FrameLog,
    upto: Location,
    caps: &ReceiverCaps,
) -> Result<(Scanned, HashMap<ObjectKey, Location>), StoreError> {
    let mut scanned = Scanned::default();
    let mut first: HashMap<ObjectKey, Location> = HashMap::new();
    log.walk(upto, &BTreeMap::new(), &mut |f| {
        scanned.add(f.len, caps)?;
        if PROOF_KINDS.contains(&f.kind) {
            first
                .try_reserve(1)
                .map_err(|_| StoreError::RedactionLimit {
                    limit: limit::FRAMES,
                })?;
            first
                .entry(ObjectKey {
                    kind: f.kind,
                    hash: f.hash,
                })
                .or_insert(f.loc);
        }
        Ok(())
    })?;
    Ok((scanned, first))
}

/// The authorization bootstrap of a holder's log: [`derive`] over its sealed batches, with the
/// carried records over it (`crate::redaction_carried::overlay`: local spans, checked as a
/// project checks them), then again over the acknowledged end that the marker rule's scan finds
/// with the derived holes, until it is stable (as the native bootstrap leaves out applies past
/// that end). Any failure refuses the open: an apply whose proof fails, a copy that its tuple
/// does not describe and an uncovered embedding are `RedactionCorrupt` at the frame; work over
/// the caps is `RedactionLimit`.
pub(crate) fn bootstrap_holder(readers: &[File]) -> Result<Authorization, StoreError> {
    let c = collect(readers)?;
    let mut upto = c.sealed_end.unwrap_or(Location {
        segment: 0,
        offset: 0,
    });
    loop {
        let auth = holder_of(&c, readers, upto, upto)?;
        let end = segment::acknowledged_end(readers, &auth.holes)?;
        if end >= upto {
            return Ok(auth);
        }
        upto = end;
    }
}

/// The holder authorization of the frames before `upto`, with the carried records before
/// `carried_upto` over it (a batch of `c.memory` included).
pub(crate) fn holder_of(
    c: &Collected,
    readers: &[File],
    upto: Location,
    carried_upto: Location,
) -> Result<Authorization, StoreError> {
    let none = BTreeMap::new();
    let mut auth = derive(
        c,
        &mut LogAccess {
            readers,
            memory: &none,
        },
        upto,
    )?;
    let mut log = LogAccess {
        readers,
        memory: &c.memory,
    };
    crate::redaction_carried::overlay(c, &mut log, &mut auth, carried_upto)?;
    Ok(auth)
}

/// The holder checks of a log whose applies are already proved (`crate::dump_scan`, which proves
/// each one as it reads it): rules 2 and 3 over every frame of `log` before `upto`
/// ([`derive_spans`]), then the carried records of `c` before `upto` laid over the spans they
/// derive (`crate::redaction_carried::overlay`), as the open checks a holder's log. `applies`
/// are the proved applies in log order, `removals` what each one approves, and `residues` the
/// residues their tuples name.
pub(crate) fn check_proved(
    c: &Collected,
    log: &mut dyn FrameLog,
    upto: Location,
    applies: &[(Location, Hash)],
    removals: BTreeMap<Hash, PortableRemoval>,
    residues: HashMap<Hash, RedactionResidue>,
) -> Result<(), StoreError> {
    let mut imported = Imported::of(removals);
    let caps = ReceiverCaps::current();
    let (holes, spans) = if applies.is_empty() {
        (Holes::new(), BTreeMap::new())
    } else {
        derive_spans(log, upto, applies, &mut imported, &residues, &caps)?
    };
    let mut auth = Authorization {
        holes,
        spans,
        residues,
        imported: Some(imported),
        ..Authorization::default()
    };
    crate::redaction_carried::overlay(c, log, &mut auth, upto)
}

/// The holder authorization of the frames before `upto` (see the module docs).
fn derive(
    c: &Collected,
    log: &mut dyn FrameLog,
    upto: Location,
) -> Result<Authorization, StoreError> {
    let requests: Vec<(Location, Hash)> = c
        .requests
        .iter()
        .filter(|(l, _)| *l < upto)
        .copied()
        .collect();
    let frames: Vec<&crate::redaction_io::ApplyFrame> =
        c.applies.iter().filter(|a| a.loc < upto).collect();
    if frames.is_empty() {
        return Ok(Authorization {
            requests,
            imported: Some(Imported::default()),
            ..Authorization::default()
        });
    }
    let caps = ReceiverCaps::current();
    let (_, first) = locate(log, upto, &caps)?;
    let mut source = LogSource {
        log: &mut *log,
        first,
    };
    // 1. every apply by its links
    let mut applies: Vec<Applied> = Vec::with_capacity(frames.len());
    let mut imported = Imported::default();
    let mut residues: HashMap<Hash, RedactionResidue> = HashMap::new();
    for a in &frames {
        let corrupt = || StoreError::corrupt_at(a.loc);
        let record = a.record.clone().ok_or_else(corrupt)?;
        let removal = sync_proof::check(&mut source, a.hash).map_err(|e| proof_error(e, a.loc))?;
        for t in removal.objects.values() {
            if let Some(r) = t.residue
                && !residues.contains_key(&r)
            {
                let key = ObjectKey {
                    kind: kinds::REDACTION_RESIDUE,
                    hash: r,
                };
                let Some(rec @ StreamRecord::Present { .. }) =
                    source.record(key).map_err(|e| proof_error(e, a.loc))?
                else {
                    return Err(corrupt());
                };
                let StreamRecord::Present { payload, .. } = &rec else {
                    unreachable!("a present record")
                };
                let res: RedactionResidue =
                    crate::codec::decode_bounded(rec.raw_kind(), payload).map_err(|_| corrupt())?;
                residues.insert(r, res);
            }
        }
        imported.removals.insert(a.hash, removal);
        applies.push(Applied {
            loc: a.loc,
            hash: a.hash,
            record,
        });
    }
    let placed: Vec<(Location, Hash)> = applies.iter().map(|a| (a.loc, a.hash)).collect();
    let (holes, spans) = derive_spans(log, upto, &placed, &mut imported, &residues, &caps)?;
    Ok(Authorization {
        holes,
        spans,
        residues,
        applies,
        requests,
        imported: Some(imported),
        carried: vec![],
    })
}

/// Rules 2 and 3 (see the module docs) over every frame of `log` before `upto`, for the proved
/// `applies` (in log order) whose removals `imported` holds: the holes and spans every copy of
/// an approved tuple gives, each request's receiver spans in `imported`, and the refusal of an
/// uncovered embedding.
fn derive_spans(
    log: &mut dyn FrameLog,
    upto: Location,
    applies: &[(Location, Hash)],
    imported: &mut Imported,
    residues: &HashMap<Hash, RedactionResidue>,
    caps: &ReceiverCaps,
) -> Result<(Holes, BTreeMap<Location, SpanAuth>), StoreError> {
    // 2. and 3. every copy of an approved tuple, and every uncovered embedding
    let mut approved: HashMap<ObjectKey, Vec<usize>> = HashMap::new();
    for (i, (_, hash)) in applies.iter().enumerate() {
        for k in imported.removals[hash].objects.keys() {
            approved.entry(*k).or_default().push(i);
        }
    }
    // every approval of a key gives it one tuple: a later apply with another length or residue
    // for a key an earlier one approves is damage at that apply
    for (k, by) in &approved {
        let first = imported.removals[&applies[by[0]].1].objects[k];
        for i in &by[1..] {
            if imported.removals[&applies[*i].1].objects[k] != first {
                return Err(StoreError::corrupt_at(applies[*i].0));
            }
        }
    }
    let versions: HashSet<Hash> = approved
        .keys()
        .filter(|k| k.kind == kinds::BLOCK)
        .map(|k| k.hash)
        .collect();
    let mut holes = Holes::new();
    let mut spans: BTreeMap<Location, SpanAuth> = BTreeMap::new();
    let mut spans_count = 0u64;
    log.walk(upto, &BTreeMap::new(), &mut |f| {
        let key = ObjectKey {
            kind: f.kind,
            hash: f.hash,
        };
        if let Some(by) = approved.get(&key) {
            let first = applies[by[0]].1;
            let removal = &imported.removals[&first];
            let tuple = removal.objects[&key];
            if f.len != tuple.len {
                return Err(StoreError::corrupt_at(f.loc));
            }
            if !copy_fits(f.kind, f.len, f.intact()?.as_deref(), &tuple, residues) {
                return Err(StoreError::corrupt_at(f.loc));
            }
            spans_count += 1;
            if spans_count > caps.spans {
                return Err(StoreError::RedactionLimit {
                    limit: limit::FRAMES,
                });
            }
            holes.insert(
                f.loc,
                Hole {
                    kind: f.kind,
                    hash: f.hash,
                    len: f.len,
                },
            );
            spans.insert(
                f.loc,
                SpanAuth {
                    residue: tuple.residue,
                    apply: first,
                    request: removal.request,
                    carried: false,
                },
            );
            for i in by {
                let r = &imported.removals[&applies[*i].1];
                imported
                    .request_spans
                    .entry(r.request)
                    .or_default()
                    .push(RedactedSpan {
                        loc: f.loc,
                        kind: f.kind,
                        hash: f.hash,
                        len: f.len,
                        residue: r.objects[&key].residue,
                    });
            }
        } else if crate::codec::logical_kind(f.kind) == kinds::CHANGE
            && !versions.is_empty()
            && f.len as usize <= MAX_RECORD_BYTES
            && let Some(p) = f.intact()?
            && let Ok(change) = crate::codec::decode_record::<Change>(f.kind, &p)
            && embeds(&change, &versions)
        {
            // an intact copy of removed content that no approval covers
            return Err(StoreError::corrupt_at(f.loc));
        }
        Ok(())
    })?;
    for v in imported.request_spans.values_mut() {
        v.sort_by_key(|s| s.loc);
        v.dedup_by_key(|s| s.loc);
    }
    Ok((holes, spans))
}

/// Whether a local copy of an approved key (rule 2) is what its approved `tuple` says: its header
/// keeps the tuple's length `len`, and its payload, when it is intact (`intact`), is what the
/// tuple's residue records (`residue_matches`, the residue decoded in `residues`). An all-zero or
/// partly zero payload is an erased or interrupted copy, and passes.
pub(crate) fn copy_fits(
    kind: u8,
    len: u32,
    intact: Option<&[u8]>,
    tuple: &crate::sync_proof::RemovalObject,
    residues: &HashMap<Hash, RedactionResidue>,
) -> bool {
    len == tuple.len
        && match (intact, tuple.residue) {
            (Some(p), Some(r)) => residues
                .get(&r)
                .is_some_and(|res| residue_matches(kind, p, res)),
            _ => true,
        }
}

/// Whether `change` creates or updates a block of a version in `versions`.
pub(crate) fn embeds(change: &Change, versions: &HashSet<Hash>) -> bool {
    change.ops.iter().any(|op| match op {
        Op::Create { block } | Op::Update { block, .. } => versions.contains(&block.version_hash()),
        Op::Delete { .. } => false,
    })
}

/// The open's refusal of a stored apply whose proof fails: an I/O failure stays one, anything
/// else is `RedactionCorrupt` at the apply.
pub(crate) fn proof_error(e: StreamError, at: Location) -> StoreError {
    if e.code == "E_IO" {
        StoreError::Io(std::io::Error::other(e.message))
    } else {
        StoreError::corrupt_at(at)
    }
}
