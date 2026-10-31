//! Related decisions (FW-EN5, FW-EN5a, D-YXBXH).
use crate::lexicon::{Lexicon, count_scan, is_tie};
use crate::query::summary_in;
use crate::{E_LIMIT, Materialized, RdyError, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, Graph};
use rdy_model::BlockId;
use rdy_model::decision::{DECISION, REFINES, REVOKES, SUPERSEDES};
use rdy_model::entity::text_sources;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// The most related decisions listed for one decision; the rest are counted.
pub const RELATED_CAP: usize = 20;
/// The most decisions one call may find through a name in their text.
pub const RELATED_TEXT_CANDIDATES: usize = 100_000;
/// The most (decision, entity, decision) incidences one call may visit through names in text.
pub const RELATED_CANDIDATE_VISITS: usize = 1_000_000;

/// The fix of both `E_LIMIT` refusals of the reader.
const LIMIT_FIX: &str =
    "brief fewer decisions, or set everyday: true on an entity that many decisions name";

/// How a related row is supplied: `link` when a lineage link supplies it, or both decisions are
/// explicitly tied to a shared entity; `text` when a decision's own words name the entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelatedVia {
    Link,
    Text,
}

impl RelatedVia {
    /// `link` or `text`, as a row shows it.
    pub fn word(self) -> &'static str {
        match self {
            RelatedVia::Link => "link",
            RelatedVia::Text => "text",
        }
    }
}

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
    /// `link` or `text`: a lineage row is always `link`; a `same_entity` row is `link` when at
    /// least one shared entity is explicitly tied at both ends (a reference, `describes` or
    /// `affects`), else `text`.
    pub via: RelatedVia,
    /// For `same_entity`, the entities both decisions are tied to or name, sorted.
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

/// The work guards of one call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RelatedLimits {
    /// The most distinct decisions found through names in text.
    pub texts: usize,
    /// The most incidences visited through names in text.
    pub visits: usize,
}

impl RelatedLimits {
    pub(crate) const DEFAULT: RelatedLimits = RelatedLimits {
        texts: RELATED_TEXT_CANDIDATES,
        visits: RELATED_CANDIDATE_VISITS,
    };
}

/// The entities a decision is tied to, and the entities its own words name.
struct Ends {
    /// Existing blocks of an entity type that the decision references or links with `describes`
    /// or `affects`.
    ties: BTreeSet<BlockId>,
    /// The entities its statement and body name by name or alias (never by key). An everyday
    /// entity is among them only when the decision is tied to it.
    named: BTreeSet<BlockId>,
}

impl Ends {
    fn all(&self) -> BTreeSet<&BlockId> {
        self.ties.iter().chain(&self.named).collect()
    }
}

/// One call's reads: each decision's ends and each entity's text candidates, worked out once.
struct Reader<'a> {
    m: &'a Materialized,
    g: &'a Graph,
    lex: &'a Lexicon,
    limits: RelatedLimits,
    ends: BTreeMap<BlockId, Rc<Ends>>,
    /// Each entity's eligible decisions whose text holds every word of one of its names.
    naming: BTreeMap<BlockId, Rc<Vec<BlockId>>>,
    found: BTreeSet<BlockId>,
    visits: usize,
}

impl<'a> Reader<'a> {
    /// Whether `id` is a decision a `same_entity` row may list: live, partly superseded or
    /// proposed.
    fn eligible(&self, id: &BlockId) -> bool {
        matches!(
            self.g.status(id),
            Some(
                DerivedStatus::Live
                    | DerivedStatus::PartiallySuperseded { .. }
                    | DerivedStatus::Proposed
            )
        ) && self.m.get(id).is_some()
    }

    /// The ties of `id` and the entities its statement and body name.
    fn ends(&mut self, id: &BlockId) -> Option<Rc<Ends>> {
        if let Some(e) = self.ends.get(id) {
            return Some(Rc::clone(e));
        }
        let b = self.m.get(id)?;
        let ties: BTreeSet<BlockId> = self
            .g
            .outgoing(id)
            .iter()
            .filter(|e| is_tie(&e.kind))
            .filter(|e| {
                self.m
                    .get(&e.to)
                    .is_some_and(|t| self.lex.is_entity_type(&t.block_type))
            })
            .map(|e| e.to.clone())
            .collect();
        let mut named = BTreeSet::new();
        // no name to find: the text is not read
        if self.lex.entity_count() > 0 {
            let source = self.lex.source(b);
            count_scan();
            for (field, text) in text_sources(b) {
                if field == "statement" || field == "body" {
                    self.lex.visit_mention_spans(&text, &source, &mut |e, _| {
                        if self.m.get(e).is_some() {
                            named.insert(e.clone());
                        }
                    });
                }
            }
        }
        let ends = Rc::new(Ends { ties, named });
        self.ends.insert(id.clone(), Rc::clone(&ends));
        Some(ends)
    }

    /// The decisions that can name `entity` in their text: a superset of those that do, which
    /// the caller checks with [`Reader::ends`]. Each one visited counts against the limits.
    fn naming(&mut self, entity: &BlockId) -> Result<Rc<Vec<BlockId>>, RdyError> {
        if let Some(n) = self.naming.get(entity) {
            return Ok(Rc::clone(n));
        }
        let list: Vec<BlockId> = self
            .lex
            .naming(entity)
            .into_iter()
            .filter(|d| self.eligible(d))
            .collect();
        let n = Rc::new(list);
        self.naming.insert(entity.clone(), Rc::clone(&n));
        Ok(n)
    }

    /// Counts one visit of text candidate `d` for an (anchor, entity) pair.
    fn visit(&mut self, d: &BlockId) -> Result<(), RdyError> {
        self.visits += 1;
        if self.visits > self.limits.visits {
            return Err(too_many(self.limits.visits, "candidate visits"));
        }
        if self.found.insert(d.clone()) && self.found.len() > self.limits.texts {
            return Err(too_many(self.limits.texts, "text candidates"));
        }
        Ok(())
    }

    fn describe(&self, id: &BlockId, for_id: &BlockId, found: Found) -> Option<RelatedDecision> {
        let b = self.m.get(id)?;
        Some(RelatedDecision {
            for_decision: for_id.to_string(),
            decision: id.to_string(),
            key: b.field_str("key").map(str::to_string),
            status: self
                .g
                .status(id)
                .as_ref()
                .map(DerivedStatus::tag)
                .unwrap_or_default()
                .into(),
            statement: match b.field_str("statement").filter(|s| !s.is_empty()) {
                Some(s) => s.to_string(),
                None => summary_in(b, self.m),
            },
            relation: found.relation.into(),
            via: found.via,
            entities: found.entities,
        })
    }
}

/// `E_LIMIT` for a call that passed `limit` of `what`.
fn too_many(limit: usize, what: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("related decisions exceed {limit} {what}"),
        LIMIT_FIX,
    )
}

/// One row before it is described.
struct Found {
    relation: &'static str,
    via: RelatedVia,
    entities: Vec<String>,
}

impl ReadState {
    /// Related decisions (exact), for each decision in `decisions` in the order given (a block
    /// that is not a decision in `scope` is ignored, and so is a repeat): the decisions it
    /// refines, supersedes or revokes and the decisions that do so to it, at any status, then the
    /// decisions that are live, partially superseded or proposed and that share an entity with
    /// it: each is tied to the entity (a reference, `describes` or `affects`) or names it in its
    /// statement or body through the name matcher. An everyday entity counts at an end only when
    /// that decision is tied to it. One entry per (for, decision, relation), by relation then
    /// decision id, at most `RELATED_CAP` per decision. The `usize` is the number of entries the
    /// cap dropped. A call past the text-candidate limits is `E_LIMIT`, never a partial list.
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
        related_in(&m, &g, &lex, decisions)
    }

    /// [`ReadState::related_decisions`] with its work limits given: a hook for tests.
    #[doc(hidden)]
    pub fn related_decisions_limited(
        &self,
        scope: &Scope,
        decisions: &[BlockId],
        texts: usize,
        visits: usize,
    ) -> Result<(Vec<RelatedDecision>, usize), RdyError> {
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let lex = self.lexicon(scope)?;
        related_with(&m, &g, &lex, decisions, RelatedLimits { texts, visits })
    }
}

/// [`ReadState::related_decisions`] over one state: `m`, its graph `g` and its lexicon `lex`.
/// Nothing outside the state it is given can change the rows.
pub(crate) fn related_in(
    m: &Materialized,
    g: &Graph,
    lex: &Lexicon,
    decisions: &[BlockId],
) -> Result<(Vec<RelatedDecision>, usize), RdyError> {
    related_with(m, g, lex, decisions, RelatedLimits::DEFAULT)
}

fn related_with(
    m: &Materialized,
    g: &Graph,
    lex: &Lexicon,
    decisions: &[BlockId],
    limits: RelatedLimits,
) -> Result<(Vec<RelatedDecision>, usize), RdyError> {
    let mut rd = Reader {
        m,
        g,
        lex,
        limits,
        ends: BTreeMap::new(),
        naming: BTreeMap::new(),
        found: BTreeSet::new(),
        visits: 0,
    };
    let is_decision = |id: &BlockId| g.status(id).is_some() && m.get(id).is_some();
    let mut out = Vec::new();
    let mut omitted = 0;
    let mut seen: BTreeSet<&BlockId> = BTreeSet::new();
    for x in decisions {
        let is_dec = m.get(x).is_some_and(|b| b.block_type == DECISION);
        if !is_dec || !seen.insert(x) {
            continue;
        }
        // (relation rank, decision) -> the row
        let mut rows: BTreeMap<(usize, BlockId), Found> = BTreeMap::new();
        for (i, (link, forward, backward)) in NEIGHBOURS.iter().enumerate() {
            for e in g.outgoing(x) {
                if matches!(&e.kind, EdgeKind::Link(n) if n == link) && is_decision(&e.to) {
                    rows.entry((2 * i, e.to.clone())).or_insert(Found {
                        relation: *forward,
                        via: RelatedVia::Link,
                        entities: vec![],
                    });
                }
            }
            for e in g.incoming(x) {
                if matches!(&e.kind, EdgeKind::Link(n) if n == link) && is_decision(&e.from) {
                    rows.entry((2 * i + 1, e.from.clone())).or_insert(Found {
                        relation: *backward,
                        via: RelatedVia::Link,
                        entities: vec![],
                    });
                }
            }
        }
        if let Some(mine) = rd.ends(x) {
            let all_mine = mine.all();
            // the candidates: each decision tied to, or naming, an entity of this decision
            let mut candidates: BTreeSet<BlockId> = BTreeSet::new();
            for ent in &all_mine {
                for e in g.incoming(ent) {
                    if e.from != *x && is_tie(&e.kind) && rd.eligible(&e.from) {
                        candidates.insert(e.from.clone());
                    }
                }
                for d in rd.naming(ent)?.iter() {
                    rd.visit(d)?;
                    if d != x {
                        candidates.insert(d.clone());
                    }
                }
            }
            for d in candidates {
                let Some(theirs) = rd.ends(&d) else { continue };
                let shared: BTreeSet<&BlockId> = all_mine
                    .iter()
                    .copied()
                    .filter(|e| theirs.ties.contains(*e) || theirs.named.contains(*e))
                    .collect();
                if shared.is_empty() {
                    continue;
                }
                let linked = mine.ties.iter().any(|e| theirs.ties.contains(e));
                rows.insert(
                    (2 * NEIGHBOURS.len(), d),
                    Found {
                        relation: SAME_ENTITY,
                        via: if linked {
                            RelatedVia::Link
                        } else {
                            RelatedVia::Text
                        },
                        entities: shared.into_iter().map(BlockId::to_string).collect(),
                    },
                );
            }
        }
        omitted += rows.len().saturating_sub(RELATED_CAP);
        for ((_, d), found) in rows.into_iter().take(RELATED_CAP) {
            out.extend(rd.describe(&d, x, found));
        }
    }
    Ok((out, omitted))
}
