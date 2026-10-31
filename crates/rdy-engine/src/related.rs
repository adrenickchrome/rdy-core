//! Related decisions (FW-EN5, FW-EN5a).
use crate::lexicon::is_tie;
use crate::query::summary_in;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, Graph};
use rdy_model::BlockId;
use rdy_model::decision::{DECISION, REFINES, REVOKES, SUPERSEDES};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The most related decisions listed for one decision; the rest are counted.
pub const RELATED_CAP: usize = 20;

/// One decision related to the decision `for_decision`, and how.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RelatedDecision {
    /// The decision asked about.
    #[serde(rename = "for")]
    pub for_decision: String,
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The related decision's derived status tag.
    pub status: String,
    pub statement: String,
    /// `refines`, `refined_by`, `supersedes`, `superseded_by`, `revokes`, `revoked_by` or
    /// `same_entity`.
    pub relation: String,
    /// For `same_entity`, the entities both decisions are tied to, sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
}

/// The neighbour relations in listing order: each link, outgoing then incoming.
const NEIGHBOURS: [(&str, &str, &str); 3] = [
    (REFINES, "refines", "refined_by"),
    (SUPERSEDES, "supersedes", "superseded_by"),
    (REVOKES, "revokes", "revoked_by"),
];
const SAME_ENTITY: &str = "same_entity";

/// The entities `id` is tied to: existing blocks of an entity type that `id` references or
/// links with `describes` or `affects`. One pass over its outgoing edges.
fn entities_of(
    g: &Graph,
    is_entity_type: &impl Fn(&str) -> bool,
    m: &Materialized,
    id: &BlockId,
) -> BTreeSet<BlockId> {
    g.outgoing(id)
        .iter()
        .filter(|e| is_tie(&e.kind))
        .filter(|e| m.get(&e.to).is_some_and(|b| is_entity_type(&b.block_type)))
        .map(|e| e.to.clone())
        .collect()
}

impl ReadState {
    /// Related decisions (exact), for each decision in `decisions` in the order given (a block
    /// that is not a decision in `scope` is ignored, and so is a repeat): the decisions it
    /// refines, supersedes or revokes and the decisions that do so to it, at any status, then the
    /// decisions that are live, partially superseded or proposed and tied to an entity it is
    /// tied to. One entry per (for, decision, relation), by relation then decision id, at most
    /// `RELATED_CAP` per decision. The `usize` is the number of entries the cap dropped.
    ///
    /// The pairs (for, decision) are the candidates M3's contradiction check (FW-G7b) reads; it is
    /// advisory (FW-G7d).
    pub fn related_decisions(
        &self,
        scope: &Scope,
        decisions: &[BlockId],
    ) -> Result<(Vec<RelatedDecision>, usize), RdyError> {
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let lex = self.lexicon(scope)?;
        Ok(related_in(&m, &g, &|t| lex.is_entity_type(t), decisions))
    }
}

/// [`ReadState::related_decisions`] over one state: `m`, its graph `g`, and the pack's entity
/// types. Nothing outside the state it is given can change the rows.
pub(crate) fn related_in(
    m: &Materialized,
    g: &Graph,
    is_entity_type: &impl Fn(&str) -> bool,
    decisions: &[BlockId],
) -> (Vec<RelatedDecision>, usize) {
    {
        let is_decision = |id: &BlockId| g.status(id).is_some() && m.get(id).is_some();
        let describe = |id: &BlockId, for_id: &BlockId, relation: &str, entities: Vec<String>| {
            let b = m.get(id)?;
            Some(RelatedDecision {
                for_decision: for_id.to_string(),
                decision: id.to_string(),
                key: b.field_str("key").map(str::to_string),
                status: g
                    .status(id)
                    .as_ref()
                    .map(DerivedStatus::tag)
                    .unwrap_or_default()
                    .into(),
                statement: match b.field_str("statement").filter(|s| !s.is_empty()) {
                    Some(s) => s.to_string(),
                    None => summary_in(b, m),
                },
                relation: relation.into(),
                entities,
            })
        };

        let mut out = Vec::new();
        let mut omitted = 0;
        let mut seen: BTreeSet<&BlockId> = BTreeSet::new();
        for x in decisions {
            let is_dec = m.get(x).is_some_and(|b| b.block_type == DECISION);
            if !is_dec || !seen.insert(x) {
                continue;
            }
            // (relation rank, decision) -> shared entities (same_entity only)
            let mut found: BTreeMap<(usize, BlockId), Vec<String>> = BTreeMap::new();
            for (i, (link, _, _)) in NEIGHBOURS.iter().enumerate() {
                for e in g.outgoing(x) {
                    if matches!(&e.kind, EdgeKind::Link(n) if n == link) && is_decision(&e.to) {
                        found.entry((2 * i, e.to.clone())).or_default();
                    }
                }
                for e in g.incoming(x) {
                    if matches!(&e.kind, EdgeKind::Link(n) if n == link) && is_decision(&e.from) {
                        found.entry((2 * i + 1, e.from.clone())).or_default();
                    }
                }
            }
            let mine = entities_of(g, is_entity_type, m, x);
            let mut shared: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
            // each incoming edge that is itself a tie: one pass over the entity's edges
            for ent in &mine {
                for e in g.incoming(ent) {
                    if e.from != *x
                        && is_tie(&e.kind)
                        && matches!(
                            g.status(&e.from),
                            Some(
                                DerivedStatus::Live
                                    | DerivedStatus::PartiallySuperseded { .. }
                                    | DerivedStatus::Proposed
                            )
                        )
                        && m.get(&e.from).is_some()
                    {
                        shared
                            .entry(e.from.clone())
                            .or_default()
                            .insert(ent.clone());
                    }
                }
            }
            for (d, ents) in shared {
                let ents = ents.iter().map(BlockId::to_string).collect();
                found.insert((2 * NEIGHBOURS.len(), d), ents);
            }

            let rows: Vec<_> = found.into_iter().collect();
            omitted += rows.len().saturating_sub(RELATED_CAP);
            for ((rank, d), ents) in rows.into_iter().take(RELATED_CAP) {
                let relation = match rank {
                    r if r < 2 * NEIGHBOURS.len() => {
                        let (_, forward, backward) = NEIGHBOURS[r / 2];
                        if r % 2 == 0 { forward } else { backward }
                    }
                    _ => SAME_ENTITY,
                };
                out.extend(describe(&d, x, relation, ents));
            }
        }
        (out, omitted)
    }
}
