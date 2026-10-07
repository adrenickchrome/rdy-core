//! cgr1: the coordination graph of the in-flight state: the in-flight changes, the open
//! proposals and the open intents, with who made each and what each writes, depends on, covers
//! or is an alternative in. Built once per published state, on first read.
use crate::error::*;
use crate::gate::{ProposalRef, open_proposals};
use crate::{Materialized, ReadState, Scope};
use rdy_graph::coord::{Coord, CoordKind, Node};
use rdy_model::decision::DECISION;
use rdy_model::{BlockId, Hash, IntentStatus, Op, Provenance};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

/// An intent's display id: `in-` and its stable id's short form.
pub fn intent_label(h: &Hash) -> String {
    format!("in-{}", h.short())
}

/// An alternative set's display id: `as-` and its stable id's short form.
pub fn alt_label(h: &Hash) -> String {
    format!("as-{}", h.short())
}

/// A state's coordination graph, and what its readers show beside it.
#[derive(Clone, Debug, Default)]
pub struct CoordIndex {
    pub graph: Coord,
    /// Each open proposal version in the graph, with its `P-<short>@<n>` reference.
    pub(crate) proposals: BTreeMap<Hash, ProposalRef>,
    /// For each in-flight change written as an alternative, the member it names.
    pub(crate) members: BTreeMap<Hash, String>,
}

/// A change's alternative link: the set's display id (`as-<short>`) and the member view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AltRef {
    pub set: String,
    pub member: String,
}

/// One in-flight change that writes a block (`rdy_context` in the `inflight` scope).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InflightChange {
    pub change: String,
    /// The open views whose set holds the change, by name.
    pub views: Vec<String>,
    pub actor: String,
    pub run: Option<String>,
    /// The intent the change was written under (`in-<short>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternative: Option<AltRef>,
    /// The open proposals that hold the change (`P-<short>@<n>`).
    pub proposals: Vec<String>,
}

/// `from -MadeBy->` the run (and the run `-RunOf->` its actor), or the actor when there is no run.
fn add_made_by(g: &mut Coord, from: Node, actor: &str, run: Option<&str>) {
    match run {
        Some(run) => {
            let r = Node::Run(actor.to_string(), run.to_string());
            g.add(from, CoordKind::MadeBy, r.clone());
            g.add(r, CoordKind::RunOf, Node::Actor(actor.to_string()));
        }
        None => g.add(from, CoordKind::MadeBy, Node::Actor(actor.to_string())),
    }
}

fn add_provenance(g: &mut Coord, from: Node, p: &Provenance) {
    add_made_by(g, from, &p.actor, p.run.as_deref());
}

impl ReadState {
    /// cgr1: the coordination graph of this state, built on first use and kept with the state.
    pub fn coord(&self) -> Result<Arc<CoordIndex>, RdyError> {
        self.coord
            .get_or_init(|| self.build_coord().map(Arc::new))
            .clone()
    }

    fn build_coord(&self) -> Result<CoordIndex, RdyError> {
        let set = self.inflight_set()?;
        let reader = self.reader();
        let mut ix = CoordIndex::default();
        let mut inflight: Option<Arc<Materialized>> = None;
        for c in &set.changes {
            let change = reader.change(c)?;
            let cn = Node::Change(*c);
            add_provenance(&mut ix.graph, cn.clone(), &change.provenance);
            for op in &change.ops {
                let b = Node::Block(op.block_id().clone());
                ix.graph.add(b, CoordKind::AuthoredIn, cn.clone());
            }
            for d in &change.deps {
                ix.graph
                    .add(cn.clone(), CoordKind::DependsOn, Node::Change(*d));
            }
            if let Some(i) = change.links.intent {
                ix.graph
                    .add(cn.clone(), CoordKind::UnderIntent, Node::Intent(i));
            }
            if let Some(a) = &change.links.alternative {
                ix.graph
                    .add(cn.clone(), CoordKind::AlternativeOf, Node::AltSet(a.set));
                ix.members.insert(*c, a.member.clone());
            }
            // a decision a create or update writes affects the areas it is in, in flight
            for op in &change.ops {
                let (Op::Create { block } | Op::Update { block, .. }) = op else {
                    continue;
                };
                if block.block_type != DECISION || self.areas.is_empty() {
                    continue;
                }
                let m = match &inflight {
                    Some(m) => m,
                    None => inflight.insert(self.materialized(&Scope::Inflight)?),
                };
                // a later in-flight change may have deleted it
                let Some(now) = m.get(&block.id) else {
                    continue;
                };
                for a in self.areas_in(m, now) {
                    let d = Node::Block(block.id.clone());
                    ix.graph.add(d, CoordKind::Affects, Node::Area(a));
                }
            }
        }
        for (pref, p) in open_proposals(self)? {
            let pn = Node::Proposal(pref.version);
            for c in &p.changes {
                ix.graph
                    .add(Node::Change(*c), CoordKind::ProposedIn, pn.clone());
            }
            add_provenance(&mut ix.graph, pn.clone(), &p.provenance);
            if let Some(a) = &p.alternative {
                ix.graph
                    .add(pn, CoordKind::AlternativeOf, Node::AltSet(a.set));
            }
            ix.proposals.insert(pref.version, pref);
        }
        // open intents whatever their expiry: readers filter by `now`
        for (stable, latest) in &self.coord_heads().intents {
            let intent = reader.intent(latest)?;
            if intent.status != IntentStatus::Open {
                continue;
            }
            let n = Node::Intent(*stable);
            for b in &intent.blocks {
                ix.graph
                    .add(n.clone(), CoordKind::Covers, Node::Block(b.clone()));
            }
            if let Some(a) = &intent.area {
                ix.graph
                    .add(n.clone(), CoordKind::Covers, Node::Area(a.clone()));
            }
            add_made_by(&mut ix.graph, n, &intent.actor, intent.run.as_deref());
        }
        Ok(ix)
    }

    /// The in-flight changes that write block `id`, ordered by change: who made each, the open
    /// views holding it, its links and the open proposals holding it.
    pub(crate) fn inflight_changes(&self, id: &BlockId) -> Result<Vec<InflightChange>, RdyError> {
        let ix = self.coord()?;
        let set = self.inflight_set()?;
        let g = &ix.graph;
        let mut out = vec![];
        for e in g.outgoing(&Node::Block(id.clone())) {
            let (CoordKind::AuthoredIn, Node::Change(c)) = (e.kind, &e.to) else {
                continue;
            };
            let mut row = InflightChange {
                change: c.to_string(),
                views: set.views_of.get(c).cloned().unwrap_or_default(),
                actor: String::new(),
                run: None,
                intent: None,
                alternative: None,
                proposals: vec![],
            };
            for e in g.outgoing(&e.to) {
                match (e.kind, &e.to) {
                    (CoordKind::MadeBy, Node::Run(actor, run)) => {
                        row.actor = actor.clone();
                        row.run = Some(run.clone());
                    }
                    (CoordKind::MadeBy, Node::Actor(actor)) => row.actor = actor.clone(),
                    (CoordKind::UnderIntent, Node::Intent(i)) => row.intent = Some(intent_label(i)),
                    (CoordKind::AlternativeOf, Node::AltSet(s)) => {
                        row.alternative = Some(AltRef {
                            set: alt_label(s),
                            member: ix.members.get(c).cloned().unwrap_or_default(),
                        })
                    }
                    (CoordKind::ProposedIn, Node::Proposal(v)) => {
                        if let Some(pref) = ix.proposals.get(v) {
                            row.proposals.push(pref.to_string());
                        }
                    }
                    _ => {}
                }
            }
            row.proposals.sort();
            out.push(row);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cevt1_labels_are_prefixed_short_hashes() {
        let h = Hash::of(b"x");
        assert_eq!(intent_label(&h), format!("in-{}", &h.to_string()[..12]));
        assert_eq!(alt_label(&h), format!("as-{}", &h.to_string()[..12]));
    }
}
