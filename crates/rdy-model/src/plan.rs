//! Change plans (D-4JMPH, D-EJBPD): the record of the work a decision change owes, the answers a
//! person gave to its items, and the seal a landing puts on it. The store keeps it as an object
//! of kind `PLAN`. Each declaration's field and variant order is its wire format (postcard):
//! never add, remove or reorder a field or a variant between two others (D-F0904).
use crate::{BlockId, Hash, Provenance, hash_of};
use serde::{Deserialize, Serialize};

/// The domain prefix of [`item_hash`].
pub const ITEM_DOMAIN: &[u8] = b"rdy-plan-item\0";

/// The prefix of a plan item's display form (`pi-` and 16 hex digits).
pub const ITEM_DISPLAY_PREFIX: &str = "pi-";

/// The most answers one plan version holds.
pub const MAX_PLAN_ANSWERS: usize = 100_000;

/// The most sealed items one plan seal holds.
pub const MAX_SEALED_ITEMS: usize = 100_000;

/// The most encoded bytes one plan version takes.
pub const MAX_PLAN_BYTES: usize = 16_777_216;

/// What a plan item asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// Change the subject to follow the decision.
    Update,
    /// Give the subject a new justification.
    Rejustify,
    /// Re-affirm the subject against the decision.
    Reaffirm,
    /// Confirm the subject as it is.
    Confirm,
    /// Change the code that implements the decision.
    UpdateImplementation,
}

impl PlanKind {
    /// The kind's name, as JSON spells it. It is also the kind's part of [`item_hash`].
    pub fn as_str(self) -> &'static str {
        match self {
            PlanKind::Update => "update",
            PlanKind::Rejustify => "rejustify",
            PlanKind::Reaffirm => "reaffirm",
            PlanKind::Confirm => "confirm",
            PlanKind::UpdateImplementation => "update_implementation",
        }
    }
}

/// What a person did to a plan item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAct {
    Confirm,
    Defer,
    Done,
}

impl PlanAct {
    /// The act's name, as JSON spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            PlanAct::Confirm => "confirm",
            PlanAct::Defer => "defer",
            PlanAct::Done => "done",
        }
    }
}

/// How a sealed plan item ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanOutcome {
    Updated,
    Relinked,
    Repinned,
    Reaffirmed,
    Retired,
    Committed,
    Confirmed,
    Done,
    Deferred,
    Open,
}

impl PlanOutcome {
    /// The outcome's name, as JSON spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            PlanOutcome::Updated => "updated",
            PlanOutcome::Relinked => "relinked",
            PlanOutcome::Repinned => "repinned",
            PlanOutcome::Reaffirmed => "reaffirmed",
            PlanOutcome::Retired => "retired",
            PlanOutcome::Committed => "committed",
            PlanOutcome::Confirmed => "confirmed",
            PlanOutcome::Done => "done",
            PlanOutcome::Deferred => "deferred",
            PlanOutcome::Open => "open",
        }
    }
}

/// The outcomes a sealed item of `kind` may have (Task 3: enum combinations; D-4JMPH): its
/// computed closes, the outcomes of the acts that may answer it, `deferred` and `open`. A
/// document subject the version deletes closes as `updated`, whatever its kind (task-6 R1, O4).
/// The writers, fsck and a holder's take check every sealed item against it.
pub fn allowed_outcomes(kind: PlanKind) -> &'static [PlanOutcome] {
    use PlanOutcome::*;
    match kind {
        PlanKind::Update => &[Updated, Deferred, Open],
        PlanKind::Rejustify => &[Relinked, Updated, Deferred, Open],
        PlanKind::Reaffirm => &[Reaffirmed, Retired, Updated, Deferred, Open],
        PlanKind::Confirm => &[Repinned, Updated, Confirmed, Deferred, Open],
        PlanKind::UpdateImplementation => &[Committed, Done, Deferred, Open],
    }
}

/// Whether a sealed item of `kind` may have `outcome` ([`allowed_outcomes`]).
pub fn outcome_allowed(kind: PlanKind, outcome: PlanOutcome) -> bool {
    allowed_outcomes(kind).contains(&outcome)
}

/// A code repository's HEAD when the plan was computed. `head` is 40 or 64 lowercase hex digits
/// (see [`valid_git_hash`]), or `None` for a repository with no commit yet (O1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanHead {
    pub repo: String,
    pub head: Option<String>,
}

/// One person's answer to one plan item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAnswer {
    /// The item's identity ([`item_hash`]).
    pub item: Hash,
    pub act: PlanAct,
    pub reason: Option<String>,
    pub subject: Option<Hash>,
    pub cause: Hash,
    pub by: Provenance,
}

/// One item as the landing sealed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedItem {
    pub id: Hash,
    pub kind: PlanKind,
    pub subject: String,
    pub cause: BlockId,
    pub edge: String,
    pub outcome: PlanOutcome,
    pub answer: Option<Hash>,
}

/// What a landing records on the plan it closes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSeal {
    pub landing: Hash,
    pub version: Hash,
    pub decisions: Vec<BlockId>,
    pub items: Vec<SealedItem>,
}

/// One version of a change plan (kind `PLAN`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The plan's first version, or `None` on that version.
    pub stable: Option<Hash>,
    /// The version this one follows, or `None` on the first.
    pub prev: Option<Hash>,
    pub view: String,
    pub proposal: Option<Hash>,
    pub answers: Vec<PlanAnswer>,
    pub heads: Vec<PlanHead>,
    pub provenance: Provenance,
    pub seal: Option<PlanSeal>,
}

impl Plan {
    /// `Hash::of` over the plan's encoding.
    pub fn id(&self) -> Hash {
        hash_of(self)
    }
}

/// A plan item's identity: BLAKE3 over [`ITEM_DOMAIN`], the cause id, a zero byte, the subject,
/// a zero byte and the kind's wire name ([`PlanKind::as_str`]), with no trailing zero.
pub fn item_hash(cause: &BlockId, subject: &str, kind: PlanKind) -> Hash {
    let cause = cause.as_str().as_bytes();
    let kind = kind.as_str().as_bytes();
    let mut bytes =
        Vec::with_capacity(ITEM_DOMAIN.len() + cause.len() + subject.len() + kind.len() + 2);
    bytes.extend_from_slice(ITEM_DOMAIN);
    bytes.extend_from_slice(cause);
    bytes.push(0);
    bytes.extend_from_slice(subject.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(kind);
    Hash::of(&bytes)
}

/// The display form of a plan item's identity: `pi-` and the first 16 lowercase hex digits. The
/// stored identity stays the full hash (p-S33GZ).
pub fn item_display(item: &Hash) -> String {
    let mut s = item.to_string();
    s.truncate(16);
    format!("{ITEM_DISPLAY_PREFIX}{s}")
}

/// Whether `s` is a Git object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex digits.
pub fn valid_git_hash(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
