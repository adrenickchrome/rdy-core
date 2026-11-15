use crate::assessment::PolicyRef;
use crate::encoding::{EncodedIdentity, LANDING_ENCODING, SNAPSHOT_DELTA_ENCODING, encoded_hash};
use crate::{AltChoice, BlockId, Hash, Provenance, hash_of};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub blocks: BTreeMap<BlockId, Hash>,
}

impl StateSnapshot {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }

    /// The canonical encoding-0 bytes of the snapshot: the postcard map, with minimal varints
    /// and the keys once each in ascending order. Its id is the hash of these bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("postcard encoding of an in-memory value cannot fail")
    }

    /// Whether `bytes`, the payload this snapshot was decoded from, are its canonical bytes,
    /// byte for byte. Postcard also decodes varints that are not minimal and keys that are
    /// repeated or out of order: such bytes give a map, but not this snapshot's identity.
    pub fn is_encoding_of(&self, bytes: &[u8]) -> bool {
        self.canonical_bytes() == bytes
    }
}

/// The id of the snapshot whose map is `blocks` ([`StateSnapshot::id`]), built without a
/// `StateSnapshot` around the map: a struct of one field encodes as that field.
pub fn snapshot_map_id(blocks: &BTreeMap<BlockId, Hash>) -> Hash {
    hash_of(blocks)
}

/// One landing's snapshot as the difference from the previous landing's map (kind
/// SNAPSHOT_DELTA, encoding [`SNAPSHOT_DELTA_ENCODING`] only). The field order is the wire
/// format.
///
/// - `landing`: the landing whose snapshot it records.
/// - `base`: the previous landing's delta (a delta hash, never a landing hash); `None` exactly
///   when the landing has no `prev`.
/// - `full`: the landing's `state`, the id of its full map ([`snapshot_map_id`]).
/// - `set`: the pairs of the full map that the previous map does not hold (a new id or a changed
///   version), sorted by id with no repeats.
/// - `drop`: the ids of the previous map that the full map does not hold, sorted with no repeats.
///
/// The first landing's previous map is empty, so its `set` is its whole map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDelta {
    pub landing: Hash,
    pub base: Option<Hash>,
    pub full: Hash,
    pub set: Vec<(BlockId, Hash)>,
    pub drop: Vec<BlockId>,
}

impl SnapshotDelta {
    /// The delta's id: the hash of its stored payload, the two-byte encoding prefix included.
    pub fn id(&self) -> Hash {
        encoded_hash(
            SNAPSHOT_DELTA_ENCODING,
            &postcard::to_allocvec(self)
                .expect("postcard encoding of an in-memory value cannot fail"),
        )
    }
}

/// Why a delta is not canonical against its previous map. Each names the first block id, in
/// check order, that breaks the form.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotDeltaError {
    #[error("set or drop is not canonical: block {0} is set twice")]
    SetRepeat(BlockId),
    #[error("set or drop is not canonical: set is not sorted at block {0}")]
    SetUnsorted(BlockId),
    #[error("set or drop is not canonical: block {0} is dropped twice")]
    DropRepeat(BlockId),
    #[error("set or drop is not canonical: drop is not sorted at block {0}")]
    DropUnsorted(BlockId),
    #[error("set or drop is not canonical: block {0} is both set and dropped")]
    Overlap(BlockId),
    #[error("set or drop is not canonical: block {0} is set to the version it already has")]
    Unchanged(BlockId),
    #[error("set or drop is not canonical: block {0} is dropped but not in the previous map")]
    AbsentDrop(BlockId),
}

/// The delta of `landing` from map `prev` to map `next`: a key-wise difference, with no read of
/// any store. `base` is the previous landing's delta (`None` for a landing with no `prev`, whose
/// `prev` map is empty). Its `set` and `drop` come out sorted by the ids' own order.
pub fn diff_snapshots(
    landing: Hash,
    base: Option<Hash>,
    prev: &BTreeMap<BlockId, Hash>,
    next: &BTreeMap<BlockId, Hash>,
) -> SnapshotDelta {
    let mut set = Vec::new();
    let mut drop = Vec::new();
    let mut old = prev.iter().peekable();
    let mut new = next.iter().peekable();
    loop {
        match (old.peek(), new.peek()) {
            (None, None) => break,
            (Some((k, _)), None) => {
                drop.push((*k).clone());
                old.next();
            }
            (None, Some((k, v))) => {
                set.push(((*k).clone(), **v));
                new.next();
            }
            (Some((ko, vo)), Some((kn, vn))) => match ko.cmp(kn) {
                Ordering::Less => {
                    drop.push((*ko).clone());
                    old.next();
                }
                Ordering::Greater => {
                    set.push(((*kn).clone(), **vn));
                    new.next();
                }
                Ordering::Equal => {
                    if vo != vn {
                        set.push(((*kn).clone(), **vn));
                    }
                    old.next();
                    new.next();
                }
            },
        }
    }
    SnapshotDelta {
        landing,
        base,
        full: snapshot_map_id(next),
        set,
        drop,
    }
}

/// Checks that `delta` is canonical against the previous map `prev`: `set` and `drop` each
/// strictly ascending (no repeat, no unsorted id), no id in both, no `set` pair that `prev`
/// already holds, and every `drop` id in `prev`. It checks the form only: `landing`, `base` and
/// `full` are the chain checks' to compare.
pub fn validate_snapshot_delta_form(
    prev: &BTreeMap<BlockId, Hash>,
    delta: &SnapshotDelta,
) -> Result<(), SnapshotDeltaError> {
    for w in delta.set.windows(2) {
        match w[0].0.cmp(&w[1].0) {
            Ordering::Less => {}
            Ordering::Equal => return Err(SnapshotDeltaError::SetRepeat(w[1].0.clone())),
            Ordering::Greater => return Err(SnapshotDeltaError::SetUnsorted(w[1].0.clone())),
        }
    }
    for w in delta.drop.windows(2) {
        match w[0].cmp(&w[1]) {
            Ordering::Less => {}
            Ordering::Equal => return Err(SnapshotDeltaError::DropRepeat(w[1].clone())),
            Ordering::Greater => return Err(SnapshotDeltaError::DropUnsorted(w[1].clone())),
        }
    }
    // both lists are sorted now: one merge walk finds a shared id
    let (mut s, mut d) = (delta.set.iter().peekable(), delta.drop.iter().peekable());
    while let (Some((ks, _)), Some(kd)) = (s.peek(), d.peek()) {
        match ks.cmp(kd) {
            Ordering::Less => {
                s.next();
            }
            Ordering::Greater => {
                d.next();
            }
            Ordering::Equal => return Err(SnapshotDeltaError::Overlap((*kd).clone())),
        }
    }
    if let Some((k, _)) = delta.set.iter().find(|(k, v)| prev.get(k) == Some(v)) {
        return Err(SnapshotDeltaError::Unchanged(k.clone()));
    }
    if let Some(k) = delta.drop.iter().find(|k| !prev.contains_key(*k)) {
        return Err(SnapshotDeltaError::AbsentDrop(k.clone()));
    }
    Ok(())
}

/// Folds `delta` into `map`, the previous landing's map: it checks the delta's form first
/// ([`validate_snapshot_delta_form`]), and on a refusal leaves `map` as it was. Then it applies
/// each `set` pair and removes each `drop` id. The caller compares the result's id with `full`.
pub fn fold_snapshot_delta(
    map: &mut BTreeMap<BlockId, Hash>,
    delta: &SnapshotDelta,
) -> Result<(), SnapshotDeltaError> {
    validate_snapshot_delta_form(map, delta)?;
    for k in &delta.drop {
        map.remove(k);
    }
    for (k, v) in &delta.set {
        map.insert(k.clone(), *v);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// The first version's id; `None` on the first version.
    pub stable: Option<Hash>,
    /// The version this one replaces; `None` on the first version.
    pub prev: Option<Hash>,
    pub title: String,
    pub view: String,
    pub changes: Vec<Hash>,
    pub alternative: Option<AltChoice>,
    pub provenance: Provenance,
    /// Canon's head when this version was written; `None` before the first landing.
    pub canon_at: Option<Hash>,
    /// The proposer's plain-words summary for the approver (FW-G7c); `None` when none was given.
    pub summary: Option<String>,
}

impl Proposal {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}

#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct LandingRecord {
    pub proposal: Hash,
    pub prev: Option<Hash>,
    pub state: Hash,
    pub approved_by: Provenance,
    pub changes: Vec<Hash>,
    pub chosen: Option<AltChoice>,
    /// The redaction requests this landing applies, sorted and distinct. Check verdicts are
    /// separate REVIEW records.
    pub checks: Vec<Hash>,
    pub assurance: Assurance,
    pub relayed_by: Option<ActorRun>,
    pub relay_evidence: Option<RelayEvidence>,
    /// The proposal version the approver approved.
    pub approved_version: Option<Hash>,
    /// The landings this one replaces after a rebase.
    pub rebased_from: Vec<Hash>,
    /// How the approval was obtained, when that is recorded.
    pub evidence: Option<ApprovalEvidence>,
    /// The ASSESSMENT of the approved version (encoding 1). None from every product writer.
    pub assessment: Option<Hash>,
    /// The authority policy block version that accepted the landing (encoding 1). Set only with
    /// assurance `policy` and no relay. Set only by the policy landing writer
    /// (`Store::prepare_land_by_policy`); None from every other product writer.
    pub policy: Option<PolicyRef>,
    /// The identity of a stored read: never serialized, `None` on every new landing. A caller
    /// that edits a stored read clears it first.
    #[serde(skip)]
    pub read_identity: Option<EncodedIdentity>,
}

impl LandingRecord {
    /// The landing's id: a stored read's recorded hash, else the hash of its latest encoding
    /// ([`LANDING_ENCODING`]: the version prefix and the body).
    pub fn id(&self) -> Hash {
        match self.read_identity {
            Some(i) => i.hash,
            None => encoded_hash(
                LANDING_ENCODING,
                &postcard::to_allocvec(self)
                    .expect("postcard encoding of an in-memory value cannot fail"),
            ),
        }
    }
}

/// Equal fields: a stored read equals the same landing built anew.
impl PartialEq for LandingRecord {
    fn eq(&self, o: &LandingRecord) -> bool {
        self.proposal == o.proposal
            && self.prev == o.prev
            && self.state == o.state
            && self.approved_by == o.approved_by
            && self.changes == o.changes
            && self.chosen == o.chosen
            && self.checks == o.checks
            && self.assurance == o.assurance
            && self.relayed_by == o.relayed_by
            && self.relay_evidence == o.relay_evidence
            && self.approved_version == o.approved_version
            && self.rebased_from == o.rebased_from
            && self.evidence == o.evidence
            && self.assessment == o.assessment
            && self.policy == o.policy
    }
}

/// How strongly a landing's approval is attested. The declaration order is the wire format
/// (postcard's variant index) and the strength order: a relay is never stronger than `Local`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Assurance {
    Policy,
    Relayed,
    Local,
}

impl Assurance {
    pub fn as_str(self) -> &'static str {
        match self {
            Assurance::Policy => "policy",
            Assurance::Relayed => "relayed",
            Assurance::Local => "local",
        }
    }
}

/// An actor and, optionally, the run it acted in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRun {
    pub actor: String,
    pub run: Option<String>,
}

/// What a relayed approval quotes, and where the quote came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayEvidence {
    pub quoted: String,
    pub session_ref: String,
    /// The question the human was asked, when the relay recorded it.
    pub question: Option<String>,
}

/// How an approval was obtained. The declaration order is the wire format (postcard's variant
/// index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    /// Asked through the client's own elicitation.
    Elicited,
    /// Answered on a rendered approval card.
    Card,
    /// Quoted from free text.
    Text,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::Elicited => "elicited",
            EvidenceKind::Card => "card",
            EvidenceKind::Text => "text",
        }
    }
}

/// The record of how a landing's approval was obtained: its kind, the options offered, the one
/// chosen, and the card shown (if any).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalEvidence {
    pub kind: EvidenceKind,
    pub options: Vec<String>,
    pub choice: Option<String>,
    pub card_hash: Option<Hash>,
}

/// A name for a landing. In rdy core every tag is a version (addendum A): it freezes the snapshot
/// its landing names. Immutable: tags are never moved or deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub landing: Hash,
    pub message: Option<String>,
    pub tagged_by: Provenance,
}

impl Tag {
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}
