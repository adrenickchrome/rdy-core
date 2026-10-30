//! Grouped plan answers (D-MPJM0, D-ME2HD, O13, O14): one answer that covers every open item a
//! group names, and the bulk repin that a `confirm` group may bring. A group is a filter on the
//! items of a change plan. It expands to one answer per matched item, so the store keeps one
//! answer per item and a plan read shows each item with its own answer.
use super::compute::{ComputedPlan, PlanCandidate};
use crate::error::{E_BAD_INPUT, E_PLAN_ITEM};
use crate::{Materialized, OpInput, RdyError};
use rdy_model::plan::PlanKind;
use rdy_model::{BlockId, Tracking};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The values `follow_up` takes, as the refusal lists them.
const FOLLOW_UPS: &str = "update, reaffirm or confirm";

/// A filter on the open items of a change plan (O13). It holds at least one field; the fields
/// AND together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanGroup {
    /// The item's kind.
    #[serde(default)]
    pub kind: Option<PlanKind>,
    /// `update`, `reaffirm`, or `confirm` (a confirm and a rejustify item).
    #[serde(default)]
    pub follow_up: Option<String>,
    /// The doc id of the item's subject in the version's final state.
    #[serde(default)]
    pub doc: Option<String>,
    /// The item's edge.
    #[serde(default)]
    pub edge: Option<String>,
}

impl PlanGroup {
    /// The kinds a `follow_up` value names, or the refusal.
    fn follow_up_kinds(&self) -> Result<Option<&'static [PlanKind]>, RdyError> {
        Ok(match self.follow_up.as_deref() {
            None => None,
            Some("update") => Some(&[PlanKind::Update]),
            Some("reaffirm") => Some(&[PlanKind::Reaffirm]),
            Some("confirm") => Some(&[PlanKind::Confirm, PlanKind::Rejustify]),
            Some(other) => {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!(
                        "group follow_up {} is not {FOLLOW_UPS}",
                        crate::views::clip(other)
                    ),
                    format!("pass {FOLLOW_UPS}"),
                ));
            }
        })
    }

    /// Whether `c` is in the group: every field the group holds agrees. `follow_up` is the
    /// kinds it names (`follow_up_kinds`), and `m` is the version's final state, for `doc`.
    fn matches(&self, c: &PlanCandidate, follow_up: Option<&[PlanKind]>, m: &Materialized) -> bool {
        self.kind.is_none_or(|k| c.kind == k)
            && follow_up.is_none_or(|ks| ks.contains(&c.kind))
            && self.edge.as_deref().is_none_or(|e| c.edge == e)
            && self.doc.as_deref().is_none_or(|d| {
                BlockId::parse(&c.subject)
                    .ok()
                    .and_then(|id| m.doc_of(&id))
                    .is_some_and(|doc| doc.id.as_str() == d)
            })
    }

    /// The open items of `computed` that the group names, in plan order. A group with no field
    /// is `E_BAD_INPUT`, an unknown `follow_up` is `E_BAD_INPUT`, and a group that matches no
    /// open item is `E_PLAN_ITEM` (O13). `open` is the ids of the open items.
    pub(crate) fn expand<'c>(
        &self,
        computed: &'c ComputedPlan,
        open: &BTreeSet<rdy_model::Hash>,
        m: &Materialized,
        scope: &str,
    ) -> Result<Vec<&'c PlanCandidate>, RdyError> {
        if self.kind.is_none()
            && self.follow_up.is_none()
            && self.doc.is_none()
            && self.edge.is_none()
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a group names at least one of kind, follow_up, doc and edge",
                "pass one of kind, follow_up, doc and edge",
            ));
        }
        let follow_up = self.follow_up_kinds()?;
        let matched: Vec<&PlanCandidate> = computed
            .items
            .iter()
            .filter(|c| open.contains(&c.id) && self.matches(c, follow_up, m))
            .collect();
        if matched.is_empty() {
            return Err(RdyError::new(
                E_PLAN_ITEM,
                format!("the group matches no open item of the change plan for {scope}"),
                "read the plan with rdy_plan / rdy plan and pass a group it holds",
            ));
        }
        Ok(matched)
    }
}

/// The repin ops of the items a `repin: true` group matched (O14): one op per subject block
/// that holds a pin to change, its `targets` the causes of its items, sorted by block. An item
/// whose subject is not a block of `m`, whose cause is gone or not cited by its subject, or
/// whose pin is already current adds no op. It still gets its answer.
pub(crate) fn repin_ops(
    m: &Materialized,
    tracking: &Tracking,
    items: &[&PlanCandidate],
) -> Vec<OpInput> {
    let mut by_block: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
    for c in items {
        let Ok(subject) = BlockId::parse(&c.subject) else {
            continue;
        };
        let (Some(block), Some(cause)) = (m.get(&subject), m.get(&c.cause)) else {
            continue;
        };
        if subject == c.cause {
            continue;
        }
        let cited = block.ref_tokens().contains(c.cause.as_str())
            || block.links.values().flatten().any(|l| l.target == c.cause);
        if cited && block.pins.get(&c.cause) != Some(&cause.content_hash_with(tracking)) {
            by_block.entry(subject).or_default().insert(c.cause.clone());
        }
    }
    by_block
        .into_iter()
        .map(|(id, causes)| OpInput::Repin {
            id: id.to_string(),
            base: None,
            targets: causes.iter().map(ToString::to_string).collect(),
            code: false,
        })
        .collect()
}
