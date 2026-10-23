//! Unjustified change: a view changes stable normative content without a justification
//! (FW-JU1, D-ZWT4H).
//!
//! The rule, per view and on the view's net change:
//! - View `v` changes block `b` when a write on `b` is live in `v` and its version differs from
//!   canon's version of `b`. A write `(c, b)` is live in `v` when `v` holds `c` and no other
//!   change of `v` that writes `b` lists `c` in its deps.
//! - `b` is justified in `v` when any change of `v` that writes `b`, live or not, carries a
//!   justification valid in `v` ([`DetectCx::justifies`]): implements or editorial, or a
//!   reflects whose every cited source `v` holds (rf10, D-4WC6R; one with no source needs
//!   `code`). A justification covers only the blocks its change writes.
//! - `b` counts when it is stable now or in canon, and a live write of `v` writes a normative
//!   block or deletes one. A delete's type comes from its base block.
//! - A decision that canon does not hold yet, or holds as proposed, never counts. So creating a
//!   decision (decide, supersede, revoke) and editing it before a landing accepts it are not
//!   unjustified changes.
//!
//! Identity: one tension per block and per set of sides. The sides are the live writes of `v` on
//! `b`. The view is not in the id, and the summary never names it. So two views that hold the
//! same live writes on `b` give one tension. A new unjustified rewrite of `b` replaces the live
//! write and gives a new id. A justified write on `b` removes the tension.
//!
//! The carry: the tension on `b` reads the writes on `b`, the facts of their changes, canon,
//! `b`'s stability and, for a reflects, whether each view holds the sources it cites. A
//! justifying write writes `b`, so `b` is a seed of its commit. A source can come or go with no
//! write on `b` (a delete, a retype, an include, an exclude, an undo, a landing), and a reflects
//! link to it can be gone too, so links are no index of the recipients: when a seed is a source
//! before or after the step, the reach is every block an in-flight change writes (rf10).
//!
//! The landing form judges the landing set's net change: the landing builds its writes with
//! every change in one view. There is no canon form (FW-CH6a): an unjustified change must never
//! become permanent, and the landing of the change resolves the tension.
use super::index::Write;
use super::{DetectCx, DetectScope, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::engine_fields;
use rdy_model::{Block, BlockId, FieldValue, Hash};
use rdy_schema::{STORE_ONLY_FIELDS, supporting};
use std::collections::BTreeSet;
use std::sync::Arc;

/// This kind's detector.
pub struct UnjustifiedChange;

/// task-5 R2 (p-0SH30, p-8FKZM): whether `new` differs from `old` only in the store-only fields
/// ([`STORE_ONLY_FIELDS`]) and in the pins its `reaffirmed` record names: the same id, type,
/// parent, order, body, links, pinned targets and every other field. A re-affirmation refreshes
/// the pin of each target it records beside the record (O18), so such a version changes no
/// content. A bare repin, whose moved pin no record names, is not one. A redacted version's
/// removed fields are unknown, so it is never one.
fn store_only_change(old: &Block, new: &Block) -> bool {
    if old.is_redacted() || new.is_redacted() {
        return false;
    }
    fn rest(b: &Block) -> impl Iterator<Item = (&String, &FieldValue)> {
        b.fields
            .iter()
            .filter(|(k, _)| !STORE_ONLY_FIELDS.contains(&k.as_str()))
    }
    let recorded: BTreeSet<BlockId> = new
        .fields
        .get(engine_fields::REAFFIRMED)
        .and_then(|v| engine_fields::decode_reaffirmed(v).ok())
        .map(|m| m.into_keys().collect())
        .unwrap_or_default();
    old.id == new.id
        && old.block_type == new.block_type
        && old.parent == new.parent
        && old.order == new.order
        && old.body == new.body
        && old.links == new.links
        && old.pins.keys().eq(new.pins.keys())
        && old
            .pins
            .iter()
            .all(|(t, h)| new.pins.get(t) == Some(h) || recorded.contains(t))
        && rest(old).eq(rest(new))
}

impl UnjustifiedChange {
    /// Whether write `w` writes a normative block, or deletes one (the type of its base block).
    fn normative(cx: &DetectCx<'_>, w: &Write) -> Result<bool, RdyError> {
        Ok(match (&w.new, w.base) {
            (Some(b), _) => cx.is_normative(b),
            (None, Some(base)) => cx.is_normative(&cx.st.reader().get_block(&base)?),
            (None, None) => false,
        })
    }

    /// Whether a block of `seeds` is a source in canon or in an in-flight write of it (as written,
    /// or as its base was). Every in-flight version of a block is one of its writes, so no
    /// materialization is read.
    fn any_source(cx: &DetectCx<'_>, seeds: &BTreeSet<BlockId>) -> Result<bool, RdyError> {
        let is_source = |b: &Block| b.block_type == supporting::SOURCE;
        let canon = cx.canon_m()?;
        for b in seeds {
            if canon.get(b).is_some_and(is_source) {
                return Ok(true);
            }
            for w in cx.writes.on(b) {
                if w.new.as_deref().is_some_and(is_source) {
                    return Ok(true);
                }
                if let Some(base) = w.base
                    && is_source(&cx.st.reader().get_block(&base)?)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// The tensions on block `b`: one per view that changes it without a justification, each set
    /// of sides once.
    fn on_block(cx: &DetectCx<'_>, b: &BlockId, out: &mut Vec<Found>) -> Result<(), RdyError> {
        let all = cx.writes.all_on(b);
        if all.is_empty() || !(cx.stable(b)? || cx.stable_before(b)?) {
            return Ok(());
        }
        let canon = cx.canon_m()?;
        let canon_version = canon.version_of(b);
        // a decision canon holds as proposed: every edit of it is part of its making. The gate
        // accepts every proposed decision a landing writes, so canon holds one as proposed only
        // when it landed outside the gate (a store-level landing, as tests make); the rule is
        // kept so such a decision is judged as one canon does not hold yet
        if canon
            .get(b)
            .is_some_and(|d| decision::status(d) == Some(DecisionStatus::Proposed))
        {
            return Ok(());
        }
        let holds = |c: &Hash, v: &str| {
            cx.writes
                .change(c)
                .is_some_and(|f| f.views.iter().any(|x| x == v))
        };
        let views: BTreeSet<&str> = all
            .iter()
            .filter_map(|w| cx.writes.change(&w.change))
            .flat_map(|f| f.views.iter().map(String::as_str))
            .collect();
        let mut seen: BTreeSet<Vec<Hash>> = BTreeSet::new();
        for v in views {
            let in_v: Vec<&Arc<Write>> = all.iter().filter(|w| holds(&w.change, v)).collect();
            // the net rule: one justification valid in this view, on any write of `b` it holds
            let mut justified = false;
            for w in &in_v {
                let j = cx
                    .writes
                    .change(&w.change)
                    .and_then(|f| f.justification.as_ref());
                if cx.justifies(j, v)? {
                    justified = true;
                    break;
                }
            }
            if justified {
                continue;
            }
            let overwritten = |w: &Write| {
                in_v.iter().any(|o| {
                    o.change != w.change
                        && cx
                            .writes
                            .change(&o.change)
                            .is_some_and(|f| f.deps.contains(&w.change))
                })
            };
            // FW-JU1 Q3: a live write that gives canon's version back (the view wrote canon's
            // text again) is no change of the block, and neither is one that differs from it
            // only in store-only fields (task-5 R2)
            let live: Vec<&Arc<Write>> = in_v
                .iter()
                .filter(|w| {
                    let store_only = match (canon.get(b), &w.new) {
                        (Some(old), Some(new)) => store_only_change(old, new),
                        _ => false,
                    };
                    !overwritten(w) && w.version != canon_version && !store_only
                })
                .copied()
                .collect();
            if live.is_empty() {
                continue;
            }
            let mut normative = false;
            for w in &live {
                if Self::normative(cx, w)? {
                    normative = true;
                    break;
                }
            }
            if !normative {
                continue;
            }
            // a decision canon does not hold yet: the view creates it
            if canon_version.is_none()
                && live.iter().any(|w| {
                    w.new
                        .as_ref()
                        .is_some_and(|n| n.block_type == decision::DECISION)
                })
            {
                continue;
            }
            let sides: Vec<Hash> = live.iter().map(|w| w.change).collect();
            if !seen.insert(sides.clone()) {
                continue;
            }
            let short: Vec<String> = sides.iter().map(|c| c.short()).collect();
            let summary = match short.as_slice() {
                [one] => {
                    format!("change {one} edits stable normative block {b} without a justification")
                }
                _ => format!(
                    "changes {} edit stable normative block {b} without a justification",
                    short.join(", ")
                ),
            };
            out.push(Found {
                kind: TensionKind::UnjustifiedChange,
                summary,
                involved: live.iter().map(|w| cx.of_write(w)).collect(),
                stable: true,
            });
        }
        Ok(())
    }
}

impl Detector for UnjustifiedChange {
    fn kind(&self) -> TensionKind {
        TensionKind::UnjustifiedChange
    }

    /// rf10: every block an in-flight change writes when a seed is a source before or after the
    /// step (in canon, or in a write of it, as written or as its base); else none. A carry reads
    /// it before and after a view op, so a source that left with an excluded change counts.
    fn reach(
        &self,
        cx: &DetectCx<'_>,
        seeds: &BTreeSet<BlockId>,
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        if cx.scope() == DetectScope::Canon || !Self::any_source(cx, seeds)? {
            return Ok(BTreeSet::new());
        }
        Ok(cx.writes.blocks().cloned().collect())
    }

    /// FW-JU1 at propose: a proposal lists only its landing form.
    fn judged_on_the_net_change(&self) -> bool {
        true
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() == DetectScope::Canon {
            return Ok(());
        }
        let blocks: BTreeSet<BlockId> = match seeds {
            Some(seeds) => seeds.clone(),
            None => cx.writes.live().map(|w| w.block.clone()).collect(),
        };
        for b in &blocks {
            Self::on_block(cx, b, out)?;
        }
        Ok(())
    }
}
