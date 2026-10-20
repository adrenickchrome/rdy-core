//! Carried removals (kind REDACTION_CARRIED, D-QF2HQ: "It never authorizes an erasure"; D-CC90E:
//! a removal needs a person's approval; D-0VG72: a holder's warning ends at a taken removal).
//!
//! A carried record explains exact zero bytes that a removal in another store left in this
//! log, and records who approved that removal. It is no approval here: it never makes a span
//! erasable, it is never a sync proof, and a fresh removal still needs a person's approval (a
//! local one in a Project, a taken one in a holder). The store reuses its removal bootstrap and
//! mask for it, so the open, a reindex, a load's ingest and the reads agree on one rule.
//!
//! # What the bootstrap accepts ([`overlay`])
//!
//! A carried record counts only from a sealed batch. It must match its hash, decode within the
//! bounded decoder's limits as its own canonical encoding, name at most 256 targets of at most
//! 256 bytes each and 1 to 100,000 spans, and keep its approver's role Approver. Each span, in
//! strictly ascending `(segment, offset)` order with no overlap, is a whole frame that ends
//! before the record's batch, of kind BLOCK, CHANGE or BLOB_CHUNK, at most 67,108,864 bytes,
//! whose header holds the span's kind, hash and length. A block or change span names a residue
//! of that object sealed before the record; a chunk span names none. A malformed record is
//! `RedactionCorrupt` at the record; a span that fails is `RedactionCorrupt` at the span.
//!
//! - `held: false`: every byte of the payload is zero. The span then joins the log's holes, with
//!   the record as its proof, while no validated apply covers it. A payload that is not all zero
//!   refuses the open: it is never erased to make it fit.
//! - `held: true`: the payload is intact (and is what its residue records), or a validated
//!   apply or a carried `held: false` span accounts for its zeros.
//!
//! A validated apply that covers a span is its proof, whatever the order; a carried record is
//! the proof only while no apply covers it. Records for the same source apply must be the same
//! record (an identical duplicate is harmless provenance), and every record that names a place
//! must give it the same kind, hash, length and residue as every other proof of it.
//!
//! The source request, apply and landing a record names need not be in this log, and they are
//! no dependency of it.
use crate::error::StoreError;
use crate::index::{IndexEntry, RedactedSpan};
use crate::kinds;
use crate::redaction::{
    MAX_TARGET_BYTES, MAX_TARGETS, is_removable, limit, residue_matches, residue_names,
    selector_len,
};
use crate::redaction_io::{Authorization, Collected, FrameLog, SpanAuth, out_of_memory};
use crate::segment::{HEADER, Hole, Holes, Location};
use crate::store::{StoreReader, decode};
use rdy_model::redaction::{
    CarriedSpan, FramePlace, MAX_RECORD_BYTES, RedactionCarried, RedactionResidue,
};
use rdy_model::{Hash, Role};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The fix of a Project's carried warning and export refusal: a fresh removal needs a person's
/// approval here.
pub const PROJECT_FIX: &str =
    "Propose the redaction of the listed targets again, and have a person approve it.";

/// The fix of a holder's carried warning and export refusal: a holder takes its removals.
pub const HOLDER_FIX: &str =
    "Ask the uploader to send the removal again, and take the upload that carries it.";

/// The most physical spans of one carried record (inclusive).
pub const MAX_CARRIED_SPANS: usize = 100_000;

/// The longest payload a carried span names, in bytes (inclusive).
pub const MAX_SPAN_LEN: u32 = 67_108_864;

/// One carried record of the log: where its frame is, its hash and the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedAt {
    pub at: Location,
    pub hash: Hash,
    pub record: RedactionCarried,
}

/// The row of the derived span index for a frame a carried record names, while no validated
/// apply covers it: the span, the carried record that is its provenance, that record's source
/// request, and the span's `held` flag. A `held: false` row masks the frame; a `held: true` row
/// masks nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarriedSpanRow {
    pub span: RedactedSpan,
    pub carried: Hash,
    pub request: Hash,
    pub held: bool,
}

fn location(p: FramePlace) -> Location {
    Location {
        segment: p.segment,
        offset: p.offset,
    }
}

/// A refused growth while a carried record decoded (`bounded::TryDecodeError::Allocation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GrowthRefused;

/// The carried record `payload` holds ([`decode_carried`]); None when it holds none, and
/// `GrowthRefused` when a growth of the decode was refused.
pub(crate) fn decode_record(payload: &[u8]) -> Result<Option<RedactionCarried>, GrowthRefused> {
    match decode_carried(payload) {
        Ok(r) => Ok(Some(r)),
        Err(crate::bounded::TryDecodeError::Allocation) => Err(GrowthRefused),
        Err(crate::bounded::TryDecodeError::Record(_)) => Ok(None),
    }
}

/// The carried record `payload` holds: at most `MAX_RECORD_BYTES` bytes, decoded by the bounded
/// decoder (HA1's limits and refusals), and exactly the canonical encoding of the record it
/// decodes to (`Record(Decode)` otherwise). Every growth of the decode is a fallible reservation
/// (`owned`), and the encoding check compares as it encodes (`bounded::encodes_as`), so a
/// refused allocation is `Allocation`, never an abort.
pub fn decode_carried(payload: &[u8]) -> Result<RedactionCarried, crate::bounded::TryDecodeError> {
    let record = crate::bounded::try_decode::<owned::Record>(payload)?.into_record()?;
    if !crate::bounded::encodes_as(&record, payload) {
        return Err(crate::bounded::RecordError::Decode.into());
    }
    Ok(record)
}

/// The fallible owned shape of a carried record: the fields of `RedactionCarried` and its parts
/// in their declared order and encoding, with each text and list grown by fallible reservation
/// (`bounded::TryString`, `bounded::TryVec`). It decodes exactly as `RedactionCarried` does (a
/// test holds them equal on records of every target form, on each damaged copy, and at each
/// limit), and moves into one.
mod owned {
    use crate::bounded::{Fallible, TryDecodeError, TryString, TryVec};
    use rdy_model::redaction::{CarriedSpan, ReasonClass, RedactTarget, RedactionCarried};
    use rdy_model::{BlockId, Hash, Provenance, Role};
    use serde::{Deserialize, Deserializer};

    #[derive(Deserialize)]
    pub(super) struct Record {
        request: Hash,
        apply: Hash,
        landing: Hash,
        approved_by: Approver,
        reason_class: ReasonClass,
        targets: TryVec<Target>,
        spans: TryVec<CarriedSpan>,
    }

    impl Fallible for Record {}

    #[derive(Deserialize)]
    struct Approver {
        actor: TryString,
        run: Option<TryString>,
        role: Role,
        at_ms: i64,
    }

    #[derive(Deserialize)]
    enum Target {
        AllVersions(Id),
        Version { id: Id, version: Hash },
        Blob(Hash),
    }

    /// A block id as `BlockId` decodes it: a text that `BlockId::parse` accepts.
    struct Id(BlockId);

    impl<'de> Deserialize<'de> for Id {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = TryString::deserialize(d)?;
            BlockId::from_string(s.0)
                .map(Id)
                .ok_or_else(|| serde::de::Error::custom("bad block id"))
        }
    }

    impl Record {
        /// The record, by moves; its target list is built again by fallible reservation.
        pub(super) fn into_record(self) -> Result<RedactionCarried, TryDecodeError> {
            let mut targets = Vec::new();
            targets
                .try_reserve_exact(self.targets.0.len())
                .map_err(|_| TryDecodeError::Allocation)?;
            targets.extend(self.targets.0.into_iter().map(|t| match t {
                Target::AllVersions(Id(id)) => RedactTarget::AllVersions(id),
                Target::Version {
                    id: Id(id),
                    version,
                } => RedactTarget::Version { id, version },
                Target::Blob(h) => RedactTarget::Blob(h),
            }));
            let a = self.approved_by;
            Ok(RedactionCarried {
                request: self.request,
                apply: self.apply,
                landing: self.landing,
                approved_by: Provenance {
                    actor: a.actor.0,
                    run: a.run.map(|r| r.0),
                    role: a.role,
                    at_ms: a.at_ms,
                },
                reason_class: self.reason_class,
                targets,
                spans: self.spans.0,
            })
        }
    }
}

/// The bounded decoder's check of a carried record `payload`, in place: the refusal
/// `bounded::decode::<RedactionCarried>` gives, or none, with no allocation (`shape`).
pub fn check_record(payload: &[u8]) -> Result<(), crate::bounded::RecordError> {
    crate::bounded::check::<shape::Record>(payload)
}

/// The in-place shapes of a carried record: the fields of `RedactionCarried` and its parts in
/// their declared order and encoding, with each text borrowed from the payload and each list
/// decoded element by element and dropped. A record decodes as its shape exactly when it
/// decodes as `RedactionCarried`, with the same refusal (a test holds them equal on records of
/// every target form, on each damaged copy of them, and at each limit).
mod shape {
    use crate::bounded::{Each, InPlace};
    use rdy_model::redaction::{CarriedSpan, ReasonClass};
    use rdy_model::{BlockId, Hash, Role};
    use serde::{Deserialize, Deserializer};

    /// The shape of `RedactionCarried`.
    pub(super) struct Record;

    impl InPlace for Record {
        fn check<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error> {
            Carried::deserialize(d).map(|_| ())
        }
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Carried<'a> {
        request: Hash,
        apply: Hash,
        landing: Hash,
        #[serde(borrow)]
        approved_by: Provenance<'a>,
        reason_class: ReasonClass,
        #[serde(borrow)]
        targets: Each<Target<'a>>,
        spans: Each<CarriedSpan>,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Provenance<'a> {
        actor: &'a str,
        #[serde(borrow)]
        run: Option<&'a str>,
        role: Role,
        at_ms: i64,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    enum Target<'a> {
        AllVersions(#[serde(borrow)] Id<'a>),
        Version {
            #[serde(borrow)]
            id: Id<'a>,
            version: Hash,
        },
        Blob(Hash),
    }

    /// A block id as `BlockId` decodes it: a text that `BlockId::parse` accepts.
    struct Id<'a>(#[allow(dead_code)] &'a str);

    impl<'de: 'a, 'a> Deserialize<'de> for Id<'a> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = <&'a str>::deserialize(d)?;
            if !BlockId::is_valid(s) {
                return Err(serde::de::Error::custom("bad block id"));
            }
            Ok(Id(s))
        }
    }
}

/// Whether a carried record's own fields keep their bounds: 1 to 100,000 spans, at most 256
/// targets of at most 256 bytes each, and an approver whose role is Approver.
fn metadata_ok(r: &RedactionCarried) -> bool {
    !r.spans.is_empty()
        && r.spans.len() <= MAX_CARRIED_SPANS
        && r.targets.len() <= MAX_TARGETS
        && r.targets
            .iter()
            .all(|t| selector_len(t) <= MAX_TARGET_BYTES)
        && r.approved_by.role == Role::Approver
}

/// The bounds a carried record keeps before it is written: at most 100,000 spans
/// (`RedactionLimit`, `100000 carried spans`), then at most 16,777,216 encoded bytes
/// (`RedactionLimit`, `16777216 bytes per record`). Returns its encoding.
pub fn check_bounds(r: &RedactionCarried) -> Result<Vec<u8>, StoreError> {
    if r.spans.len() > MAX_CARRIED_SPANS {
        return Err(StoreError::RedactionLimit {
            limit: limit::CARRIED_SPANS,
        });
    }
    let too_big = || StoreError::RedactionLimit {
        limit: limit::RECORD,
    };
    let mut buf = rdy_model::redaction::record_buffer().map_err(|_| too_big())?;
    let bytes = rdy_model::redaction::encode_into(r, &mut buf)
        .map_err(|_| too_big())?
        .to_vec();
    Ok(bytes)
}

/// The holes the carried `held: false` spans of the records before `below` would add, unchecked:
/// what the marker rule's scan reads as frames while the bootstrap finds the acknowledged end.
pub(crate) fn candidate_holes(c: &Collected, below: Location, holes: &mut Holes) {
    for f in c.carried.iter().filter(|f| f.loc < below) {
        let Some(r) = &f.record else { continue };
        for s in r.spans.iter().filter(|s| !s.held) {
            holes.entry(location(s.place)).or_insert(Hole {
                kind: s.kind,
                hash: s.hash,
                len: s.len,
            });
        }
    }
}

/// The hash key of place `l` (a `Location` is ordered, not hashed).
fn key(l: Location) -> (u32, u64) {
    (l.segment, l.offset)
}

/// One span of a carried record checked against the log.
struct Identity {
    kind: u8,
    hash: Hash,
    len: u32,
    residue: Option<Hash>,
}

/// Validates every carried record of `c` before `upto` (see the module docs) and lays it over
/// `auth`, whose applies are already validated: each `held: false` span that no apply covers
/// joins `auth.holes` with the record as its proof (`SpanAuth::carried`), each residue a span
/// names joins `auth.residues`, and the records are kept in `auth.carried` when `log` keeps them
/// ([`FrameLog::keeps_carried`]). Each record is read through `log`
/// ([`FrameLog::carried_record`]) once per step, one at a time, and only the facts the checks
/// need are kept: the places the records name, one identity per place, the held places with
/// zero bytes, and one record hash per source apply; each grows by fallible reservation (an
/// I/O error of kind `OutOfMemory`). Any other failure is `RedactionCorrupt`: at the record for a
/// malformed record, at the span for a span.
pub(crate) fn overlay(
    c: &Collected,
    log: &mut dyn FrameLog,
    auth: &mut Authorization,
    upto: Location,
) -> Result<(), StoreError> {
    let frames = || c.carried.iter().filter(move |f| f.loc < upto);
    if frames().next().is_none() {
        return Ok(());
    }
    // 1. each record's own shape, and one record per source apply
    let mut by_apply: HashMap<Hash, Hash> = HashMap::new();
    let mut wanted: HashSet<(u32, u64)> = HashSet::new();
    let mut walk_to = Location {
        segment: 0,
        offset: 0,
    };
    for f in frames() {
        let at = || StoreError::corrupt_at(f.loc);
        let r = log.carried_record(f)?.ok_or_else(at)?;
        if !metadata_ok(&r) {
            return Err(at());
        }
        match by_apply.get(&r.apply) {
            Some(h) if *h != f.hash => return Err(at()),
            Some(_) => {}
            None => {
                by_apply.try_reserve(1).map_err(|_| out_of_memory())?;
                by_apply.insert(r.apply, f.hash);
            }
        }
        let mut prev: Option<(Location, u64)> = None;
        for s in &r.spans {
            let loc = location(s.place);
            let bad = || StoreError::corrupt_at(loc);
            if !is_removable(s.kind) || s.len > MAX_SPAN_LEN {
                return Err(bad());
            }
            match (s.kind, s.residue) {
                (kinds::BLOB_CHUNK, None) => {}
                (kinds::BLOCK | kinds::CHANGE, Some(_)) => {}
                _ => return Err(bad()),
            }
            if let Some((p, end)) = prev
                && (loc <= p || (loc.segment == p.segment && loc.offset < end))
            {
                return Err(bad());
            }
            let end = loc
                .offset
                .checked_add(HEADER)
                .and_then(|e| e.checked_add(u64::from(s.len)))
                .ok_or_else(bad)?;
            let before_batch = loc.segment < f.batch_start.segment
                || (loc.segment == f.batch_start.segment && end <= f.batch_start.offset);
            if !before_batch {
                return Err(bad());
            }
            prev = Some((loc, end));
            wanted.try_reserve(1).map_err(|_| out_of_memory())?;
            wanted.insert(key(loc));
        }
        walk_to = walk_to.max(f.batch_start);
    }
    // 2. every span is a frame start of the log: the first place no frame starts at fails
    log.walk(walk_to, &BTreeMap::new(), &mut |seen| {
        wanted.remove(&key(seen.loc));
        Ok(())
    })?;
    if let Some((segment, offset)) = wanted.iter().min() {
        return Err(StoreError::RedactionCorrupt {
            segment: *segment,
            offset: *offset,
        });
    }
    drop(wanted);
    // 3. each span against its frame, its residue and every other proof of its place
    let mut identity: HashMap<(u32, u64), Identity> = HashMap::new();
    identity
        .try_reserve(auth.holes.len())
        .map_err(|_| out_of_memory())?;
    for (loc, h) in &auth.holes {
        identity.insert(
            key(*loc),
            Identity {
                kind: h.kind,
                hash: h.hash,
                len: h.len,
                residue: auth.spans.get(loc).and_then(|s| s.residue),
            },
        );
    }
    let mut held_zero = HeldZero::default();
    let mut kept: Vec<CarriedAt> = vec![];
    for f in frames() {
        let r = log
            .carried_record(f)?
            .ok_or_else(|| StoreError::corrupt_at(f.loc))?;
        for s in &r.spans {
            check_span(c, log, auth, &mut identity, &mut held_zero, f, &r, s)?;
        }
        if log.keeps_carried() {
            kept.try_reserve(1).map_err(|_| out_of_memory())?;
            kept.push(CarriedAt {
                at: f.loc,
                hash: f.hash,
                record: r.into_owned(),
            });
        }
    }
    // a held payload that is zero: some proof of its place accounts for its zeros
    if let Some(loc) = held_zero.order.iter().find(|l| !auth.holes.contains_key(l)) {
        return Err(StoreError::corrupt_at(*loc));
    }
    auth.carried = kept;
    Ok(())
}

/// The places of `held: true` spans whose payload is zero, each once, in the order the checks
/// first met them.
#[derive(Default)]
struct HeldZero {
    order: Vec<Location>,
    seen: HashSet<(u32, u64)>,
}

impl HeldZero {
    fn note(&mut self, loc: Location) -> Result<(), StoreError> {
        if self.seen.contains(&key(loc)) {
            return Ok(());
        }
        self.seen.try_reserve(1).map_err(|_| out_of_memory())?;
        self.order.try_reserve(1).map_err(|_| out_of_memory())?;
        self.seen.insert(key(loc));
        self.order.push(loc);
        Ok(())
    }
}

/// Checks span `s` of carried record `r` (frame `f`): its header, its residue, its identity
/// against every other proof of its place, then its payload by its `held` flag. A `held: false`
/// span that no apply covers joins the holes; a zero `held: true` span is noted in `held_zero`.
#[allow(clippy::too_many_arguments)]
fn check_span(
    c: &Collected,
    log: &mut dyn FrameLog,
    auth: &mut Authorization,
    identity: &mut HashMap<(u32, u64), Identity>,
    held_zero: &mut HeldZero,
    f: &crate::redaction_io::CarriedFrame,
    r: &RedactionCarried,
    s: &CarriedSpan,
) -> Result<(), StoreError> {
    let loc = location(s.place);
    let bad = || StoreError::corrupt_at(loc);
    if log.header_at(loc)? != Some((s.len, s.kind, s.hash)) {
        return Err(bad());
    }
    // the residue: one of this object, sealed before the record
    let residue = match s.residue {
        Some(h) => {
            let res = match auth.residues.get(&h) {
                Some(res) => res.clone(),
                None => {
                    let rloc = c
                        .residue_at
                        .get(&h)
                        .filter(|l| **l < f.loc)
                        .ok_or_else(bad)?;
                    let payload = log
                        .record(*rloc, kinds::REDACTION_RESIDUE, &h)?
                        .ok_or_else(bad)?;
                    decode::<RedactionResidue>(&payload).map_err(|_| bad())?
                }
            };
            if !residue_names(&res, s.kind, &s.hash) {
                return Err(bad());
            }
            Some((h, res))
        }
        None => None,
    };
    // one identity per place, across applies and carried records
    match identity.get(&key(loc)) {
        Some(i) => {
            if (i.kind, i.hash, i.len, i.residue) != (s.kind, s.hash, s.len, s.residue) {
                return Err(bad());
            }
        }
        None => {
            identity.try_reserve(1).map_err(|_| out_of_memory())?;
            identity.insert(
                key(loc),
                Identity {
                    kind: s.kind,
                    hash: s.hash,
                    len: s.len,
                    residue: s.residue,
                },
            );
        }
    }
    if auth.erasable(&loc) {
        // a validated apply covers it, and is its proof
        return Ok(());
    }
    let zero = log.payload_is_zero(loc, s.len)?;
    if !s.held {
        if !zero {
            return Err(bad());
        }
        // the first record that accounts for the zeros is their proof
        if let std::collections::btree_map::Entry::Vacant(e) = auth.holes.entry(loc) {
            e.insert(Hole {
                kind: s.kind,
                hash: s.hash,
                len: s.len,
            });
            auth.spans.insert(
                loc,
                SpanAuth {
                    residue: s.residue,
                    apply: f.hash,
                    request: r.request,
                    carried: true,
                },
            );
        }
        if let Some((h, res)) = residue {
            keep_residue(auth, h, res)?;
        }
        return Ok(());
    }
    let payload = log.payload(loc, s.len)?;
    if Hash::of(&payload) == s.hash {
        // intact: what its residue records
        if let Some((_, res)) = &residue
            && !residue_matches(s.kind, &payload, res)
        {
            return Err(bad());
        }
        return Ok(());
    }
    if !zero {
        return Err(bad());
    }
    held_zero.note(loc)?;
    if let Some((h, res)) = residue {
        keep_residue(auth, h, res)?;
    }
    Ok(())
}

/// Keeps residue `res` (hash `h`) in `auth` unless it holds one, by fallible growth.
fn keep_residue(
    auth: &mut Authorization,
    h: Hash,
    res: RedactionResidue,
) -> Result<(), StoreError> {
    if !auth.residues.contains_key(&h) {
        auth.residues.try_reserve(1).map_err(|_| out_of_memory())?;
        auth.residues.insert(h, res);
    }
    Ok(())
}

/// The index entry of a validated carried record.
pub(crate) fn entry_of(c: &CarriedAt) -> IndexEntry {
    IndexEntry::RedactionCarried {
        carried: c.hash,
        request: c.record.request,
        loc: c.at,
        spans: spans_of(&c.record),
    }
}

/// A record's spans as index rows, each with its `held` flag.
fn spans_of(r: &RedactionCarried) -> Vec<(RedactedSpan, bool)> {
    r.spans
        .iter()
        .map(|s| {
            (
                RedactedSpan {
                    loc: location(s.place),
                    kind: s.kind,
                    hash: s.hash,
                    len: s.len,
                    residue: s.residue,
                },
                s.held,
            )
        })
        .collect()
}

impl StoreReader {
    /// Calls `f` with every carried record frame (kind REDACTION_CARRIED) of the sealed log that
    /// lies wholly before `end`, in physical order. The records are found through the index's
    /// rows of their places (one per physical record, derived from the validated records and
    /// checked against the log at every open), and each is read by its place, so the cost is
    /// that of the carried records alone: no walk of the log's frames and no scan of every
    /// stored object. Each physical copy of an identical record is visited at its own place (a
    /// warning deduplicates by source apply itself). Every such frame was validated by the open
    /// (`overlay`); a frame whose header, kind or hash no longer matches its row, a payload that
    /// no longer matches its hash, or one that decodes to no carried record is
    /// `RedactionCorrupt` at its frame.
    pub fn visit_carried(
        &self,
        end: Location,
        f: &mut dyn FnMut(CarriedAt) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.refuse_pending()?;
        let upto = end.min(self.index().indexed_upto()?);
        self.index().for_each_carried_place(upto, |loc, hash| {
            let bad = || StoreError::corrupt_at(loc);
            let corrupt = |e: std::io::Error| match e.kind() {
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => bad(),
                _ => StoreError::Io(e),
            };
            self.note_carried_frame_read();
            let (len, kind, h) = self
                .segments()
                .header(loc)
                .map_err(corrupt)?
                .ok_or_else(bad)?;
            if kind != kinds::REDACTION_CARRIED || h != hash || len as usize > MAX_RECORD_BYTES {
                return Err(bad());
            }
            // a frame that crosses `end` lies after it
            let frame_end = loc.offset + HEADER + u64::from(len);
            if loc.segment == upto.segment && frame_end > upto.offset {
                return Ok(());
            }
            let (_, _, payload) = self.segments().read_hashed(loc).map_err(corrupt)?;
            let record = decode_record(&payload)
                .map_err(|_| out_of_memory())?
                .ok_or_else(bad)?;
            f(CarriedAt {
                at: loc,
                hash,
                record,
            })
        })
    }

    /// The carried row of the derived span index at `loc`, if a carried record is the proof or
    /// the provenance of that frame (no validated apply covers it).
    pub fn carried_span_at(&self, loc: Location) -> Result<Option<CarriedSpanRow>, StoreError> {
        Ok(self.index().redaction_span_row_at(&loc)?.and_then(|row| {
            row.held.map(|held| CarriedSpanRow {
                span: row.active.span,
                carried: row.active.apply,
                request: row.active.request,
                held,
            })
        }))
    }

    /// Of `record`'s physical spans: how many the store still holds, and how many are removed
    /// (a validated apply or a carried `held: false` span masks it).
    pub fn carried_counts(&self, record: &RedactionCarried) -> Result<(u64, u64), StoreError> {
        let (mut held, mut removed) = (0u64, 0u64);
        for s in &record.spans {
            if self
                .index()
                .redaction_span_at(&location(s.place))?
                .is_some()
            {
                removed += 1;
            } else {
                held += 1;
            }
        }
        Ok((held, removed))
    }
}
