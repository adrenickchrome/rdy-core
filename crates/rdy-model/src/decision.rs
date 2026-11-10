use crate::foreign::{ForeignId, ForeignPin};
use crate::{Block, Tracking};
use std::collections::BTreeMap;

pub const DECISION: &str = "decision";
pub const QUESTION: &str = "question";

pub const SUPERSEDES: &str = "supersedes";
pub const REFINES: &str = "refines";
pub const DEPENDS_ON: &str = "depends_on";
pub const CONDITIONAL_ON: &str = "conditional_on";
pub const REVOKES: &str = "revokes";
pub const ANSWERS: &str = "answers";
pub const DECIDED_BY: &str = "decided_by";
pub const IMPLEMENTS: &str = "implements";
pub const DESCRIBES: &str = "describes";
pub const AFFECTS: &str = "affects";

/// The decision field naming when a decision takes effect (RFC 3339 UTC, like `recorded_at`).
pub const EFFECTIVE_FROM: &str = "effective_from";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionStatus {
    Proposed,
    Accepted,
    Rejected,
}

pub fn status(b: &Block) -> Option<DecisionStatus> {
    if b.block_type != DECISION {
        return None;
    }
    match b.field_str("status")? {
        "proposed" => Some(DecisionStatus::Proposed),
        "accepted" => Some(DecisionStatus::Accepted),
        "rejected" => Some(DecisionStatus::Rejected),
        _ => None,
    }
}

/// The fields of a decision that make up its ruling (with its links): never tracking fields.
pub const RULING_FIELDS: [&str; 3] = ["statement", "options", "status"];

/// trk1 (D-DZC1J): `new` differs from `old` in at least one tracking field of `old`'s type and in
/// nothing else: the same id, type, parent, order, body and links, and every non-tracking field
/// equal. Pins are compared, and so is the whole foreign part: no tracking edit moves a foreign
/// link or pin.
///
/// A redacted block's removed fields are unknown, so a write over one is never tracking-only.
pub fn only_tracking_changed(old: &Block, new: &Block, t: &Tracking) -> bool {
    if old.is_redacted() || new.is_redacted() {
        return false;
    }
    let Some(tracking) = t.fields(&old.block_type) else {
        return false;
    };
    if old.id != new.id
        || old.block_type != new.block_type
        || old.parent != new.parent
        || old.order != new.order
        || old.body != new.body
        || old.links != new.links
        || old.pins != new.pins
        || old.foreign != new.foreign
    {
        return false;
    }
    let untracked = |(k, _): &(&String, _)| !tracking.contains(k.as_str());
    let old_rest = old.fields.iter().filter(untracked);
    old_rest.eq(new.fields.iter().filter(untracked))
        && tracking
            .iter()
            .any(|f| old.fields.get(f) != new.fields.get(f))
}

/// D-EJBPD (p-QMGB4, p-HF5W0): `new` re-affirms `old`. It differs from `old` only in the
/// store-only `reaffirmed` field and in the hashes of pins `old` already holds: the same id,
/// type, parent, order, body, links, pinned targets, foreign part (links and pins alike: a
/// re-affirmation never moves a foreign pin) and every other field. A re-affirmation changes no
/// content, so a frozen decision may take one.
///
/// A redacted block's removed fields are unknown, so a write over one is never a re-affirmation.
pub fn only_reaffirmation_changed(old: &Block, new: &Block) -> bool {
    use crate::engine_fields::REAFFIRMED;
    if old.is_redacted() || new.is_redacted() {
        return false;
    }
    let rest = |b: &Block| {
        b.fields
            .iter()
            .filter(|(k, _)| k.as_str() != REAFFIRMED)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>()
    };
    old.id == new.id
        && old.block_type == new.block_type
        && old.parent == new.parent
        && old.order == new.order
        && old.body == new.body
        && old.links == new.links
        && old.pins.keys().eq(new.pins.keys())
        && old.foreign == new.foreign
        && rest(old) == rest(new)
        && new.fields.contains_key(REAFFIRMED)
}

/// The foreign pin moves one adopt answer binds on one block (repo links, D-8VJAZ): for each
/// cited target, the pin the block holds and the pin the adoption moves it to.
pub type PinMoves = BTreeMap<ForeignId, (ForeignPin, ForeignPin)>;

/// Repo links (D-8VJAZ): `new` differs from `old` only by the foreign pin moves `moves`, each
/// exactly: every target of `moves` is pinned by `old` with its `from` pin and by `new` with its
/// `to` pin, which differs, and every other foreign pin is equal. The same id, type, parent,
/// order, body, links, local pins, foreign links and every field, tracking, ruling and engine
/// fields alike: a tracking edit or a re-affirmation never rides an adoption. An empty `moves`
/// never matches.
///
/// It describes a shape only. A frozen decision takes such a write only with the authority of
/// the person's adopt answer that bound these moves (the store's `PinAdoption`).
///
/// A redacted block's removed fields are unknown, so a write over one is never an adoption.
pub fn only_foreign_pins_adopted(old: &Block, new: &Block, moves: &PinMoves) -> bool {
    if old.is_redacted() || new.is_redacted() || moves.is_empty() {
        return false;
    }
    if old.id != new.id
        || old.block_type != new.block_type
        || old.parent != new.parent
        || old.order != new.order
        || old.body != new.body
        || old.links != new.links
        || old.pins != new.pins
        || old.fields != new.fields
        || old.foreign.links != new.foreign.links
        || !old.foreign.pins.keys().eq(new.foreign.pins.keys())
    {
        return false;
    }
    if moves
        .iter()
        .any(|(t, (from, to))| from == to || old.foreign.pins.get(t) != Some(from))
    {
        return false;
    }
    old.foreign
        .pins
        .iter()
        .all(|(t, pin)| match (moves.get(t), new.foreign.pins.get(t)) {
            (Some((_, to)), Some(now)) => now == to,
            (None, Some(now)) => now == pin,
            (_, None) => false,
        })
}

/// An accepted or rejected decision. A redacted block keeps the frozen fact its marker
/// recorded; removing a body never unfreezes a decision.
pub fn is_frozen(b: &Block) -> bool {
    if let Some(r) = &b.read_identity {
        return r.frozen;
    }
    matches!(
        status(b),
        Some(DecisionStatus::Accepted | DecisionStatus::Rejected)
    )
}

/// When a decision takes effect: its `effective_from`, otherwise when it was recorded. `None` for
/// a block that is not a decision.
pub fn effective_from(b: &Block) -> Option<&str> {
    if b.block_type != DECISION {
        return None;
    }
    b.field_str(EFFECTIVE_FROM).or(b.field_str("recorded_at"))
}
