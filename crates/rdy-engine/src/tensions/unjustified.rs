//! Unjustified change: a change edits stable normative content without a justification.
//!
//! The trigger is the change itself, so there is no canon form (FW-CH6a): an unjustified change
//! must never become permanent, and the landing of the change resolves the tension.
use super::{DetectCx, DetectScope, Detector, Found, Involved, TensionKind};
use crate::RdyError;
use rdy_model::{BlockId, Hash};
use std::collections::BTreeSet;

/// This kind's detector.
pub struct UnjustifiedChange;

impl UnjustifiedChange {
    /// The involved items of change `c`: one per block it writes (every op, live or not) that is
    /// normative and stable now or in canon. A delete's type comes from its base block.
    fn items(cx: &DetectCx<'_>, c: &Hash) -> Result<Vec<Involved>, RdyError> {
        let mut items = vec![];
        for w in cx.writes.of_change(c) {
            let normative = match (&w.new, w.base) {
                (Some(b), _) => cx.is_normative(b),
                (None, Some(base)) => cx.is_normative(&cx.st.reader().get_block(&base)?),
                (None, None) => false,
            };
            if normative && (cx.stable(&w.block)? || cx.stable_before(&w.block)?) {
                items.push(cx.of_write(&w));
            }
        }
        Ok(items)
    }
}

impl Detector for UnjustifiedChange {
    fn kind(&self) -> TensionKind {
        TensionKind::UnjustifiedChange
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
        let changes: BTreeSet<Hash> = match seeds {
            None => cx.writes.changes().map(|f| f.id).collect(),
            Some(seeds) => seeds
                .iter()
                .flat_map(|s| cx.writes.all_on(s))
                .map(|w| w.change)
                .collect(),
        };
        for c in changes {
            if cx.writes.change(&c).is_none_or(|f| f.justified) {
                continue;
            }
            let involved = Self::items(cx, &c)?;
            if involved.is_empty() {
                continue;
            }
            let ids: Vec<String> = involved.iter().map(|i| i.block.to_string()).collect();
            out.push(Found {
                kind: TensionKind::UnjustifiedChange,
                summary: format!(
                    "change {} edits stable normative content without a justification: {}",
                    c.short(),
                    ids.join(", ")
                ),
                involved,
                stable: true,
            });
        }
        Ok(())
    }
}
