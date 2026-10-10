//! Unjustified change: a view changes stable normative content without a justification
//! (FW-JU1, D-ZWT4H).
//!
//! The rule, per view and on the view's net change:
//! - View `v` changes block `b` when a write on `b` is live in `v` and its version differs from
//!   canon's version of `b`. A write `(c, b)` is live in `v` when `v` holds `c` and no other
//!   change of `v` that writes `b` lists `c` in its deps.
//! - `b` is justified in `v` when any change of `v` that writes `b`, live or not, carries a
//!   justification (implements or editorial). A justification names decisions, never blocks, so
//!   it covers only the blocks its change writes.
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
//! The carry: the tension on `b` reads only the writes on `b`, the facts of their changes, canon
//! and `b`'s stability. A justifying write writes `b`, so `b` is a seed of its commit. The
//! detector needs no reach.
//!
//! The landing form judges the landing set's net change: the landing builds its writes with
//! every change in one view. There is no canon form (FW-CH6a): an unjustified change must never
//! become permanent, and the landing of the change resolves the tension.
use super::index::Write;
use super::{DetectCx, DetectScope, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{BlockId, Hash};
use std::collections::BTreeSet;
use std::sync::Arc;

/// This kind's detector.
pub struct UnjustifiedChange;

impl UnjustifiedChange {
    /// Whether write `w` writes a normative block, or deletes one (the type of its base block).
    fn normative(cx: &DetectCx<'_>, w: &Write) -> Result<bool, RdyError> {
        Ok(match (&w.new, w.base) {
            (Some(b), _) => cx.is_normative(b),
            (None, Some(base)) => cx.is_normative(&cx.st.reader().get_block(&base)?),
            (None, None) => false,
        })
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
            if in_v
                .iter()
                .any(|w| cx.writes.change(&w.change).is_some_and(|f| f.justified))
            {
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
            // text again) is no change of the block
            let live: Vec<&Arc<Write>> = in_v
                .iter()
                .filter(|w| !overwritten(w) && w.version != canon_version)
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
