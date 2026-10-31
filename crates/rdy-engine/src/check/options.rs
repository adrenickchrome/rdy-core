//! The landing projection (D-JQD5X, review I1): the canon a version's landing gives, with the
//! acceptance change's deterministic updates applied: each written, surviving proposed decision
//! accepted, the considered options of a choice written into its decision (D-8GR43), and the key
//! and placement rewrites of K1 and D-TX3KH. Readiness reads it before its rule checks and card
//! facts; the landing builds its acceptance change from it. Both read the same function, so the
//! transformations exist once.
//!
//! It reads captured state, writes nothing and invents no clock value: the landing alone stamps
//! `decided_at` on the accepted decisions.
use crate::alt::alt_label;
use crate::error::{E_ALT_DECISION, E_STALE_BASE, RdyError};
use crate::gate::{ProposalRef, applied_ops, changes_of, rekeyable_in};
use crate::materialize::Materialized;
use crate::{ReadState, Scope};
use rdy_model::{BlockId, Change};
use rdy_model::{FieldValue, Hash, Op, Proposal, decision};
use std::collections::BTreeSet;

/// The card fact that lists the considered options, after `Decisions`.
pub const CONSIDERED_OPTIONS_FACT: &str = "Considered options";
/// The card fact that binds the alternative set's version, after `Considered options`.
pub const ALTERNATIVE_SET_FACT: &str = "Alternative set";

/// What a choosing version's landing writes into a decision's `options`: the set, its version
/// now, the decision, and the full list (the authored options in their order, then each
/// unchosen member). Never a stored record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsideredOptions {
    pub set: Hash,
    pub set_version: Hash,
    pub decision: BlockId,
    pub entries: Vec<String>,
}

impl ConsideredOptions {
    /// The `Considered options` fact's value: the options the landing writes, as a compact JSON
    /// array in their order.
    pub fn fact_value(&self) -> String {
        serde_json::to_string(&self.entries).expect("a list of strings serializes")
    }

    /// The `Alternative set` fact's value: `<set label>@<full set version hash>`.
    pub fn set_value(&self) -> String {
        format!("{}@{}", alt_label(&self.set), self.set_version)
    }
}

/// The landing projection of a version: the projected canon, the acceptance change's
/// deterministic updates (one per block, each on its authored base, before the landing clock is
/// added) and the considered options of its choice.
#[derive(Clone, Debug)]
pub struct LandingProjection {
    pub candidate: Materialized,
    pub updates: Vec<Op>,
    pub considered: Option<ConsideredOptions>,
}

/// The decision blocks `changes` create or update.
fn written_decisions(changes: &[Change]) -> BTreeSet<BlockId> {
    let mut ids = BTreeSet::new();
    for c in changes {
        for op in &c.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op
                && block.block_type == decision::DECISION
            {
                ids.insert(block.id.clone());
            }
        }
    }
    ids
}

/// The considered options of version `pref` (record `p`) in its authored prospective canon
/// `candidate` (D-8GR43, O11). None when the version chooses nothing. The target is the
/// proposed decision the version's changes create, still in `candidate`, that has an outgoing
/// `answers` link to the set's question; else the only proposed decision they create. A canon
/// decision is never a target. Two answers, no created decision, or several without one answer
/// is `E_ALT_DECISION`. Each entry reads `<label> (<set label>/<view>, not chosen)`, for each
/// unchosen member in the set's member order, after the target's authored options.
pub fn considered_options(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
    candidate: &Materialized,
) -> Result<Option<ConsideredOptions>, RdyError> {
    let Some(choice) = &p.alternative else {
        return Ok(None);
    };
    let (stable, version, set) = st.resolve_alt(&choice.set.to_string())?;
    let label = alt_label(&stable);
    let changes = changes_of(st, &p.changes)?;
    let mut created = BTreeSet::new();
    for c in &changes {
        for op in &c.ops {
            if let Op::Create { block } = op
                && block.block_type == decision::DECISION
            {
                created.insert(block.id.clone());
            }
        }
    }
    let proposed: Vec<&rdy_model::Block> = created
        .iter()
        .filter_map(|id| candidate.get(id))
        .filter(|b| decision::status(b) == Some(decision::DecisionStatus::Proposed))
        .collect();
    let answering: Vec<&&rdy_model::Block> = match &set.question {
        Some(q) => proposed
            .iter()
            .filter(|b| {
                b.links
                    .get(decision::ANSWERS)
                    .is_some_and(|ls| ls.iter().any(|l| l.target == *q))
            })
            .collect(),
        None => vec![],
    };
    let target = match (answering.as_slice(), proposed.as_slice()) {
        ([one], _) => *one,
        ([], [only]) => only,
        _ => {
            return Err(RdyError::new(
                E_ALT_DECISION,
                format!(
                    "{pref} chooses in {label} but names no decision for the considered options"
                ),
                "answer the set's question with one decision, or create exactly one decision: rdy_decide / rdy new decision",
            ));
        }
    };
    let mut entries: Vec<String> = match target.fields.get("options") {
        Some(FieldValue::List(items)) => items
            .iter()
            .filter_map(|i| match i {
                FieldValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    entries.extend(
        set.members
            .iter()
            .filter(|m| m.view != choice.member)
            .map(|m| format!("{} ({label}/{}, not chosen)", m.label, m.view)),
    );
    Ok(Some(ConsideredOptions {
        set: stable,
        set_version: version,
        decision: target.id.clone(),
        entries,
    }))
}

/// The landing projection of version `pref` (record `p`), whose authored prospective canon is
/// `authored` (canon with its changes, in landing order). The refusals keep the landing's order:
/// a proposed decision, then a re-keyed block, that changed in the view after the proposal
/// (`E_STALE_BASE`, the first of them); a kept or re-keyed placement's own refusal; then the
/// considered options' `E_ALT_DECISION`.
pub fn landing_projection(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
    authored: &Materialized,
) -> Result<LandingProjection, RdyError> {
    let changes = changes_of(st, &p.changes)?;
    let applied = st.landing_order(&changes);
    let ops = applied_ops(&changes, &applied);
    let canon = st.materialized(&Scope::Canon)?;
    let vm = st.materialized(&Scope::View(p.view.clone()))?;
    let mut updates: Vec<Op> = vec![];
    let mut stale = vec![];
    // each written proposed decision is accepted; its view must not have moved on from it
    for id in written_decisions(&changes) {
        let Some(b) = authored.get(&id) else {
            continue;
        };
        if decision::status(b) != Some(decision::DecisionStatus::Proposed) {
            continue;
        }
        if vm.get(&id).map(|v| v.version_hash()) != Some(b.version_hash()) {
            stale.push(RdyError::new(
                E_STALE_BASE,
                format!(
                    "decision {id} changed in view {:?} after the proposal was made",
                    p.view
                ),
                format!("the view has later edits to {id}; propose again to include them"),
            ));
            continue;
        }
        let mut acc = b.clone();
        acc.fields
            .insert("status".into(), FieldValue::Str("accepted".into()));
        updates.push(Op::Update {
            base: b.version_hash(),
            block: acc,
        });
    }
    // K1 (FW-PR3): a block landed since the proposal can hold a key rdy derived for a block this
    // version creates; the landing re-derives it, past the keys the view holds
    let re = crate::keys::rederive(authored, &rekeyable_in(&changes, &applied, &canon), &vm);
    for (id, k) in &re {
        let b = authored
            .get(id)
            .expect("a re-derived block is in the prospective canon");
        if vm.get(id).map(|v| v.version_hash()) != Some(b.version_hash()) {
            stale.push(RdyError::new(
                E_STALE_BASE,
                format!(
                    "{id} needs a new key (another block holds its key now), but it changed in view {:?} after the proposal was made",
                    p.view
                ),
                format!("the view has later edits to {id}; propose again to include them"),
            ));
            continue;
        }
        let key = FieldValue::Str(k.clone());
        match find(&mut updates, id) {
            Some(block) => {
                block.fields.insert("key".into(), key);
            }
            None => {
                let mut rekeyed = b.clone();
                rekeyed.fields.insert("key".into(), key);
                updates.push(Op::Update {
                    base: b.version_hash(),
                    block: rekeyed,
                });
            }
        }
    }
    if let Some(e) = stale.into_iter().next() {
        return Err(e);
    }
    // D-TX3KH: a block this version places that would share its order key with a sibling gets a
    // key of its own. D-1FT5Y: a block whose op kept its base's order key, where canon's version
    // differs from that base only by its key, lands with canon's key; it is no placed block, and
    // the re-key reads the candidate with canon's keys.
    let kept = crate::landing_order::kept_places(st.reader(), &canon, authored, &ops)?;
    let with_kept;
    let placing = if kept.is_empty() {
        authored
    } else {
        let restored: Vec<Op> = kept
            .iter()
            .map(|(id, k)| {
                let b = authored
                    .get(id)
                    .expect("a kept block is in the prospective canon");
                let mut at = b.clone();
                at.order = k.clone();
                Op::Update {
                    base: b.version_hash(),
                    block: at,
                }
            })
            .collect();
        with_kept = authored.with(&restored);
        &with_kept
    };
    let mut placed = crate::landing_order::placed(&canon, &ops);
    placed.retain(|id| !kept.contains_key(id));
    let mut order_keys = crate::landing_order::rekeys(&canon, placing, &vm, &placed)?;
    order_keys.extend(kept);
    for (id, k) in &order_keys {
        let b = authored
            .get(id)
            .expect("a re-keyed block is in the prospective canon");
        if vm.get(id).map(|v| v.version_hash()) != Some(b.version_hash()) {
            return Err(crate::landing_order::stale_error(id, &p.view));
        }
        match find(&mut updates, id) {
            Some(block) => block.order = k.clone(),
            None => {
                let mut rekeyed = b.clone();
                rekeyed.order = k.clone();
                updates.push(Op::Update {
                    base: b.version_hash(),
                    block: rekeyed,
                });
            }
        }
    }
    // D-8GR43: the considered options join the target decision's acceptance update
    let considered = considered_options(st, pref, p, authored)?;
    if let Some(c) = &considered {
        let list = FieldValue::List(c.entries.iter().cloned().map(FieldValue::Str).collect());
        match find(&mut updates, &c.decision) {
            Some(block) => {
                block.fields.insert("options".into(), list);
            }
            None => {
                let b = authored
                    .get(&c.decision)
                    .expect("the target decision is in the prospective canon");
                let mut with = b.clone();
                with.fields.insert("options".into(), list);
                updates.push(Op::Update {
                    base: b.version_hash(),
                    block: with,
                });
            }
        }
    }
    let candidate = if updates.is_empty() {
        authored.clone()
    } else {
        authored.with(&updates)
    };
    Ok(LandingProjection {
        candidate,
        updates,
        considered,
    })
}

/// The block of the update of `id` among `updates`, if any: each block has one update.
fn find<'u>(updates: &'u mut [Op], id: &BlockId) -> Option<&'u mut rdy_model::Block> {
    updates.iter_mut().find_map(|op| match op {
        Op::Update { block, .. } if block.id == *id => Some(block),
        _ => None,
    })
}
