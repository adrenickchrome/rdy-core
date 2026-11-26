//! The stored forms of a plan version and the one bounded fold that hydrates them (D-3PNRK,
//! D-4JMPH, p-CST21). A PLAN frame is encoding 0 (the frozen whole body) or encoding 1
//! (`rdy_model::plan_encoding::PlanRecordV1`: a whole version or a delta of the version before
//! it). [`decode`] reads one frame within the record bounds and fetches nothing; [`fold`] hydrates
//! a version from its frame back to the nearest whole version.
//!
//! # The fold
//!
//! A delta names the version immediately before it and the chain's first version. The fold
//! first collects the hashes from the asked version back to the nearest whole version, at most
//! [`MAX_PLAN_DELTA_DEPTH`] deltas and one whole version; it then reads that whole version and
//! applies each delta in forward order to one answer map, one record at a time. It keeps no
//! decoded record but the one it applies, so it reads at most 258 records. A cycle, a missing
//! predecessor, a predecessor of another kind, another chain's first version, an unsorted,
//! repeated or overlapping answer list, a set answer equal to the one it replaces, a removal of
//! an item the predecessor does not answer and a 129th delta are `plan <hash>: invalid chain`;
//! a bound past the reconstructed answer or byte limit is `plan <hash>: invalid fields`. No
//! partial answers are returned.
//!
//! A walk that needs only metadata (the last bound proposal, whether a chain ended, the
//! approved binding's search, the delta depth of a new version) decodes each frame it visits
//! once with [`decode`] and never folds.
use crate::bounded::{self, RecordError};
use crate::error::StoreError;
use crate::kinds;
use crate::plan::{INVALID_CHAIN, INVALID_FIELDS, damage};
use rdy_model::encoding::{EncodedIdentity, PLAN_DELTA_ENCODING, encoded_hash};
use rdy_model::plan::{MAX_PLAN_ANSWERS, MAX_PLAN_BYTES, Plan, PlanAnswer, PlanHead, PlanSeal};
use rdy_model::plan_encoding::{MAX_PLAN_DELTA_DEPTH, PlanDeltaV1, PlanRecordV1, PlanV0};
use rdy_model::{Hash, Provenance};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

/// One decoded PLAN frame: a whole version (its identity set) or a delta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanRecord {
    Full(Plan),
    Delta(PlanDeltaV1),
}

impl PlanRecord {
    pub fn stable(&self) -> Option<Hash> {
        match self {
            PlanRecord::Full(p) => p.stable,
            PlanRecord::Delta(d) => Some(d.stable),
        }
    }

    pub fn prev(&self) -> Option<Hash> {
        match self {
            PlanRecord::Full(p) => p.prev,
            PlanRecord::Delta(d) => Some(d.prev),
        }
    }

    pub fn view(&self) -> &str {
        match self {
            PlanRecord::Full(p) => &p.view,
            PlanRecord::Delta(d) => &d.view,
        }
    }

    pub fn proposal(&self) -> Option<Hash> {
        match self {
            PlanRecord::Full(p) => p.proposal,
            PlanRecord::Delta(d) => d.proposal,
        }
    }

    pub fn seal(&self) -> Option<&PlanSeal> {
        match self {
            PlanRecord::Full(p) => p.seal.as_ref(),
            PlanRecord::Delta(d) => d.seal.as_ref(),
        }
    }

    pub fn heads(&self) -> &[PlanHead] {
        match self {
            PlanRecord::Full(p) => &p.heads,
            PlanRecord::Delta(d) => &d.heads,
        }
    }

    pub fn provenance(&self) -> &Provenance {
        match self {
            PlanRecord::Full(p) => &p.provenance,
            PlanRecord::Delta(d) => &d.provenance,
        }
    }

    /// The answers the frame itself holds: a whole version's answers, a delta's set answers.
    pub fn explicit_answers(&self) -> &[PlanAnswer] {
        match self {
            PlanRecord::Full(p) => &p.answers,
            PlanRecord::Delta(d) => &d.answers_set,
        }
    }

    pub fn is_full(&self) -> bool {
        matches!(self, PlanRecord::Full(_))
    }

    /// The record's own fields as a plan, its answers the frame's explicit ones: what the rules
    /// of one record (`crate::plan::invalid_record`) check of a frame alone. A delta's answer
    /// list is a subset of its version's.
    pub(crate) fn as_record_plan(&self) -> Plan {
        match self {
            PlanRecord::Full(p) => p.clone(),
            PlanRecord::Delta(d) => Plan {
                stable: Some(d.stable),
                prev: Some(d.prev),
                view: d.view.clone(),
                proposal: d.proposal,
                answers: d.answers_set.clone(),
                heads: d.heads.clone(),
                provenance: d.provenance.clone(),
                seal: d.seal.clone(),
                read_identity: None,
            },
        }
    }
}

thread_local! {
    static DECODES: Cell<u64> = const { Cell::new(0) };
    static FOLDS: Cell<u64> = const { Cell::new(0) };
}

/// How many PLAN frames this thread decoded ([`decode`]) and how many folds it ran ([`fold`])
/// since [`reset_counts_for_tests`]: a test's counting reader.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanReadCounts {
    pub decodes: u64,
    pub folds: u64,
}

#[doc(hidden)]
pub fn counts_for_tests() -> PlanReadCounts {
    PlanReadCounts {
        decodes: DECODES.with(Cell::get),
        folds: FOLDS.with(Cell::get),
    }
}

#[doc(hidden)]
pub fn reset_counts_for_tests() {
    DECODES.with(|c| c.set(0));
    FOLDS.with(|c| c.set(0));
}

/// The PLAN frame of raw kind `raw` and stored payload `payload`, decoded within its bounds: a
/// version-0 or version-1 whole version (with its frame's identity), or a version-1 delta. It
/// fetches no predecessor. A frame of another kind is `Kind`; bytes that are not the record's
/// own canonical encoding are `Decode`.
pub fn decode(raw: u8, payload: &[u8]) -> Result<(PlanRecord, EncodedIdentity), RecordError> {
    if payload.len() > MAX_PLAN_BYTES + 2 {
        return Err(RecordError::Limit {
            name: bounded::RECORD_BYTES,
            maximum: MAX_PLAN_BYTES as u64,
        });
    }
    let o = crate::codec::split(raw, payload)?;
    if o.kind != kinds::PLAN {
        return Err(RecordError::Kind);
    }
    decode_body(o.version, o.body)
}

/// [`decode`] of a split frame: encoding `version` and body `body`.
pub fn decode_body(
    version: u16,
    body: &[u8],
) -> Result<(PlanRecord, EncodedIdentity), RecordError> {
    DECODES.with(|c| c.set(c.get() + 1));
    if body.len() > MAX_PLAN_BYTES {
        return Err(RecordError::Limit {
            name: bounded::RECORD_BYTES,
            maximum: MAX_PLAN_BYTES as u64,
        });
    }
    let identity = EncodedIdentity {
        hash: encoded_hash(version, body),
        version,
    };
    let record = match version {
        0 => {
            let p: PlanV0 = bounded::decode(body)?;
            if !bounded::encodes_as(&p, body) {
                return Err(RecordError::Decode);
            }
            PlanRecord::Full(p.into_current(Some(identity)))
        }
        PLAN_DELTA_ENCODING => {
            let r: PlanRecordV1 = bounded::decode(body)?;
            if !bounded::encodes_as(&r, body) {
                return Err(RecordError::Decode);
            }
            match r {
                PlanRecordV1::Full(p) => PlanRecord::Full(p.into_current(Some(identity))),
                PlanRecordV1::Delta(d) => PlanRecord::Delta(d),
            }
        }
        _ => return Err(RecordError::UnknownVersion),
    };
    Ok((record, identity))
}

/// Why a fold stopped: a record breaks the chain rule (`plan`, the record whose rule broke, and
/// the reason), or a read failed.
#[derive(Debug)]
pub enum FoldError {
    Broken { plan: Hash, reason: &'static str },
    Store(StoreError),
}

impl From<StoreError> for FoldError {
    fn from(e: StoreError) -> Self {
        FoldError::Store(e)
    }
}

impl FoldError {
    /// The store error of a read: a broken rule is `Decode` with `plan <hash>: <reason>`.
    pub fn into_store(self) -> StoreError {
        match self {
            FoldError::Broken { plan, reason } => StoreError::Decode(damage(&plan, reason)),
            FoldError::Store(e) => e,
        }
    }
}

/// The encoding and body of the PLAN `h`, or None when no PLAN of that hash is there.
pub type ReadFrame<'a> = dyn Fn(&Hash) -> Result<Option<(u16, Vec<u8>)>, StoreError> + 'a;

fn broken(plan: Hash, reason: &'static str) -> FoldError {
    FoldError::Broken { plan, reason }
}

/// The record `h` as `read` gives it, decoded: None when it is missing.
fn record_of(read: &ReadFrame<'_>, h: &Hash) -> Result<Option<PlanRecord>, FoldError> {
    let Some((version, body)) = read(h)? else {
        return Ok(None);
    };
    match decode_body(version, &body) {
        Ok((r, id)) if id.hash == *h => Ok(Some(r)),
        // a frame that does not decode, or holds other bytes than its hash names
        _ => Err(broken(*h, INVALID_FIELDS)),
    }
}

/// The postcard size of `v`.
fn size_of<T: serde::Serialize>(v: &T) -> usize {
    postcard::experimental::serialized_size(v).unwrap_or(usize::MAX)
}

/// Hydrates the plan version `target` through `read`: its whole version, or the whole version
/// it last follows with each delta after it applied in order (see the module docs). The value
/// keeps `target`'s identity. A record that does not decode is `invalid fields` at its hash; a
/// broken chain is `invalid chain` at the delta whose rule it breaks.
pub fn fold(target: &Hash, read: &ReadFrame<'_>) -> Result<Plan, FoldError> {
    FOLDS.with(|c| c.set(c.get() + 1));
    // the hashes back to the whole version: at most MAX_PLAN_DELTA_DEPTH deltas and one
    let mut chain: Vec<Hash> = Vec::new();
    let mut seen: HashSet<Hash> = HashSet::new();
    let mut at = *target;
    let full = loop {
        let Some(r) = record_of(read, &at)? else {
            return Err(match chain.last() {
                Some(child) => broken(*child, INVALID_CHAIN),
                None => FoldError::Store(StoreError::NotFound(at)),
            });
        };
        chain.push(at);
        seen.insert(at);
        match r {
            PlanRecord::Full(p) if chain.len() == 1 => return Ok(p),
            PlanRecord::Full(p) => break p,
            PlanRecord::Delta(d) => {
                if chain.len() > MAX_PLAN_DELTA_DEPTH {
                    return Err(broken(*target, INVALID_CHAIN));
                }
                if seen.contains(&d.prev) {
                    return Err(broken(at, INVALID_CHAIN));
                }
                at = d.prev;
            }
        }
    };
    let full_hash = chain.pop().expect("the whole version's hash");
    let stable = full.stable.unwrap_or(full_hash);
    let mut answers: BTreeMap<Hash, PlanAnswer> = BTreeMap::new();
    let mut answer_bytes = 0usize;
    for a in full.answers {
        answer_bytes = answer_bytes.saturating_add(size_of(&a));
        answers.insert(a.item, a);
    }
    let mut prev = full_hash;
    let mut last: Option<(Hash, PlanDeltaV1)> = None;
    // forward order: the delta right after the whole version first
    while let Some(h) = chain.pop() {
        let Some(PlanRecord::Delta(d)) = record_of(read, &h)? else {
            return Err(broken(h, INVALID_CHAIN));
        };
        apply(&h, prev, stable, &d, &mut answers, &mut answer_bytes)?;
        prev = h;
        // only the asked version's own metadata is kept
        last = Some((
            h,
            PlanDeltaV1 {
                answers_set: vec![],
                answers_remove: vec![],
                ..d
            },
        ));
    }
    let Some((h, d)) = last else {
        unreachable!("a delta was asked for");
    };
    debug_assert_eq!(h, *target);
    let plan = Plan {
        stable: Some(d.stable),
        prev: Some(d.prev),
        view: d.view,
        proposal: d.proposal,
        answers: answers.into_values().collect(),
        heads: d.heads,
        provenance: d.provenance,
        seal: d.seal,
        read_identity: Some(EncodedIdentity {
            hash: *target,
            version: PLAN_DELTA_ENCODING,
        }),
    };
    if size_of(&plan) > MAX_PLAN_BYTES {
        return Err(broken(*target, INVALID_FIELDS));
    }
    Ok(plan)
}

/// Applies delta `d` (hash `h`) to `answers`, the answers of its predecessor `prev` in chain
/// `stable`: every list rule and bound is checked before the map changes.
fn apply(
    h: &Hash,
    prev: Hash,
    stable: Hash,
    d: &PlanDeltaV1,
    answers: &mut BTreeMap<Hash, PlanAnswer>,
    answer_bytes: &mut usize,
) -> Result<(), FoldError> {
    if d.prev != prev || d.stable != stable || !d.lists_valid() {
        return Err(broken(*h, INVALID_CHAIN));
    }
    // the projected count and bytes, before any insertion
    let mut count = answers.len();
    let mut bytes = *answer_bytes;
    for item in &d.answers_remove {
        let Some(old) = answers.get(item) else {
            return Err(broken(*h, INVALID_CHAIN));
        };
        count -= 1;
        bytes = bytes.saturating_sub(size_of(old));
    }
    for a in &d.answers_set {
        match answers.get(&a.item) {
            Some(old) if old == a => return Err(broken(*h, INVALID_CHAIN)),
            Some(old) => bytes = bytes.saturating_sub(size_of(old)),
            None => count += 1,
        }
        bytes = bytes.saturating_add(size_of(a));
    }
    if count > MAX_PLAN_ANSWERS || bytes > MAX_PLAN_BYTES {
        return Err(broken(*h, INVALID_FIELDS));
    }
    for item in &d.answers_remove {
        answers.remove(item);
    }
    for a in &d.answers_set {
        answers.insert(a.item, a.clone());
    }
    *answer_bytes = bytes;
    Ok(())
}

/// How many consecutive deltas the version `h` ends: 0 for a whole version, else the deltas
/// from `h` back to the nearest whole version. It reads metadata only, each frame once, and
/// stops after [`MAX_PLAN_DELTA_DEPTH`] + 1 delta headers. None when a record on the way is
/// missing or does not decode: no delta can follow it.
pub(crate) fn delta_depth(h: &Hash, read: &ReadFrame<'_>) -> Result<Option<usize>, StoreError> {
    let mut at = *h;
    let mut depth = 0usize;
    loop {
        let r = match record_of(read, &at) {
            Ok(Some(r)) => r,
            Ok(None) | Err(FoldError::Broken { .. }) => return Ok(None),
            Err(FoldError::Store(e)) => return Err(e),
        };
        match r {
            PlanRecord::Full(_) => return Ok(Some(depth)),
            PlanRecord::Delta(d) => {
                depth += 1;
                if depth > MAX_PLAN_DELTA_DEPTH {
                    return Ok(Some(depth));
                }
                at = d.prev;
            }
        }
    }
}

/// The metadata record `h` as `read` gives it, for a walk that reads metadata only: None when it
/// is missing or does not decode (which ends such a walk).
pub(crate) fn meta(h: &Hash, read: &ReadFrame<'_>) -> Result<Option<PlanRecord>, StoreError> {
    match record_of(read, h) {
        Ok(r) => Ok(r),
        Err(FoldError::Broken { .. }) => Ok(None),
        Err(FoldError::Store(e)) => Err(e),
    }
}
