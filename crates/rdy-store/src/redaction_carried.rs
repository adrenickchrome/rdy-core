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
use crate::redaction_io::{Authorization, Collected, SpanAuth, for_each_seen, read_record};
use crate::segment::{HEADER, Hole, Holes, Location, header_at, payload_is_zero};
use crate::store::{StoreReader, decode, encode};
use rdy_model::redaction::{
    CarriedSpan, FramePlace, MAX_RECORD_BYTES, RedactionCarried, RedactionResidue,
};
use rdy_model::{Hash, Role};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;

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

fn location(p: FramePlace) -> Location {
    Location {
        segment: p.segment,
        offset: p.offset,
    }
}

/// The carried record `payload` holds: at most `MAX_RECORD_BYTES` bytes, decoded by the bounded
/// decoder, and exactly the canonical encoding of the record it decodes to. None otherwise.
pub(crate) fn decode_record(payload: &[u8]) -> Option<RedactionCarried> {
    if payload.len() > MAX_RECORD_BYTES {
        return None;
    }
    let record = crate::bounded::decode::<RedactionCarried>(payload).ok()?;
    (encode(&record) == payload).then_some(record)
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
/// names joins `auth.residues`, and the records are kept in `auth.carried`. Any failure is
/// `RedactionCorrupt`: at the record for a malformed record, at the span for a span.
pub(crate) fn overlay(
    c: &Collected,
    readers: &[File],
    auth: &mut Authorization,
    upto: Location,
) -> Result<(), StoreError> {
    let frames: Vec<_> = c.carried.iter().filter(|f| f.loc < upto).collect();
    if frames.is_empty() {
        return Ok(());
    }
    // 1. each record's own shape, and one record per source apply
    let mut by_apply: HashMap<Hash, Hash> = HashMap::new();
    let mut wanted: BTreeSet<Location> = BTreeSet::new();
    let mut walk_to = Location {
        segment: 0,
        offset: 0,
    };
    for f in &frames {
        let at = || StoreError::corrupt_at(f.loc);
        let r = f.record.as_ref().ok_or_else(at)?;
        if !metadata_ok(r) {
            return Err(at());
        }
        match by_apply.get(&r.apply) {
            Some(h) if *h != f.hash => return Err(at()),
            Some(_) => {}
            None => {
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
            wanted.insert(loc);
        }
        walk_to = walk_to.max(f.batch_start);
    }
    // 2. every span is a frame start of the log
    let mut found: BTreeSet<Location> = BTreeSet::new();
    for_each_seen(readers, walk_to, &BTreeMap::new(), |seen| {
        if wanted.contains(&seen.loc) {
            found.insert(seen.loc);
        }
        Ok(())
    })?;
    if let Some(loc) = wanted.iter().find(|l| !found.contains(l)) {
        return Err(StoreError::corrupt_at(*loc));
    }
    // 3. each span against its frame, its residue and every other proof of its place
    let mut identity: BTreeMap<Location, Identity> = auth
        .holes
        .iter()
        .map(|(loc, h)| {
            (
                *loc,
                Identity {
                    kind: h.kind,
                    hash: h.hash,
                    len: h.len,
                    residue: auth.spans.get(loc).and_then(|s| s.residue),
                },
            )
        })
        .collect();
    let mut held_zero: Vec<Location> = vec![];
    let mut kept: Vec<CarriedAt> = Vec::with_capacity(frames.len());
    for f in &frames {
        let r = f.record.as_ref().expect("a checked record");
        for s in &r.spans {
            check_span(c, readers, auth, &mut identity, &mut held_zero, f, r, s)?;
        }
        kept.push(CarriedAt {
            at: f.loc,
            hash: f.hash,
            record: r.clone(),
        });
    }
    // a held payload that is zero: some proof of its place accounts for its zeros
    if let Some(loc) = held_zero.iter().find(|l| !auth.holes.contains_key(l)) {
        return Err(StoreError::corrupt_at(*loc));
    }
    auth.carried = kept;
    Ok(())
}

/// Checks span `s` of carried record `r` (frame `f`): its header, its residue, its identity
/// against every other proof of its place, then its payload by its `held` flag. A `held: false`
/// span that no apply covers joins the holes; a zero `held: true` span is noted in `held_zero`.
#[allow(clippy::too_many_arguments)]
fn check_span(
    c: &Collected,
    readers: &[File],
    auth: &mut Authorization,
    identity: &mut BTreeMap<Location, Identity>,
    held_zero: &mut Vec<Location>,
    f: &crate::redaction_io::CarriedFrame,
    r: &RedactionCarried,
    s: &CarriedSpan,
) -> Result<(), StoreError> {
    let loc = location(s.place);
    let bad = || StoreError::corrupt_at(loc);
    let file = readers.get(loc.segment as usize).ok_or_else(bad)?;
    if header_at(file, loc.offset, file.metadata()?.len())? != Some((s.len, s.kind, s.hash)) {
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
                    let payload =
                        read_record(readers, &c.memory, *rloc, kinds::REDACTION_RESIDUE, &h)?
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
    match identity.get(&loc) {
        Some(i) => {
            if (i.kind, i.hash, i.len, i.residue) != (s.kind, s.hash, s.len, s.residue) {
                return Err(bad());
            }
        }
        None => {
            identity.insert(
                loc,
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
    let start = loc.offset + HEADER;
    let zero = payload_is_zero(file, start, u64::from(s.len))?;
    if !s.held {
        if !zero {
            return Err(bad());
        }
        if !auth.holes.contains_key(&loc) {
            auth.holes.insert(
                loc,
                Hole {
                    kind: s.kind,
                    hash: s.hash,
                    len: s.len,
                },
            );
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
            auth.residues.entry(h).or_insert(res);
        }
        return Ok(());
    }
    let payload = crate::redaction_io::read_payload(file, start, s.len)?;
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
    held_zero.push(loc);
    if let Some((h, res)) = residue {
        auth.residues.entry(h).or_insert(res);
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

/// The index entry of the carried record frame `hash` at `loc` with `payload`. A payload that is
/// not a carried record is `RedactionCorrupt` at it.
pub(crate) fn carried_entry(
    hash: Hash,
    loc: Location,
    payload: &[u8],
) -> Result<IndexEntry, StoreError> {
    let record = decode_record(payload).ok_or_else(|| StoreError::corrupt_at(loc))?;
    Ok(IndexEntry::RedactionCarried {
        carried: hash,
        request: record.request,
        loc,
        spans: spans_of(&record),
    })
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
    /// Calls `f` with every carried record whose frame lies before `end`, in physical order. An
    /// identical duplicate record is one record, at its first frame. Reads the typed index and
    /// each record's frame; keeps no table of its own.
    pub fn visit_carried(
        &self,
        end: Location,
        f: &mut dyn FnMut(CarriedAt) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut found: Vec<(Location, Hash)> = vec![];
        self.index()
            .for_each_object_of(kinds::REDACTION_CARRIED, |h| {
                if let Some(at) = self.index().object(&h)?
                    && at < end
                {
                    found.push((at, h));
                }
                Ok(())
            })?;
        found.sort();
        for (at, hash) in found {
            let payload = self.get_kind(kinds::REDACTION_CARRIED, &hash)?;
            let record = decode_record(&payload).ok_or_else(|| StoreError::corrupt_at(at))?;
            f(CarriedAt { at, hash, record })?;
        }
        Ok(())
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
