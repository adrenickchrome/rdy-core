use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{Block, BlockId, Hash};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum EdgeKind {
    Contains,
    References,
    Link(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Edge {
    pub from: BlockId,
    pub to: BlockId,
    pub kind: EdgeKind,
    pub part: Option<String>,
    pub pin: Option<Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DerivedStatus {
    Proposed,
    Rejected,
    Live,
    PartiallySuperseded { by: Vec<BlockId>, parts: Vec<String> },
    Superseded { by: Vec<BlockId> },
    Revoked { by: Vec<BlockId> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Suspect {
    pub block: BlockId,
    pub target: BlockId,
    pub reason: String,
}

#[derive(Default)]
pub struct Graph {
    out: BTreeMap<BlockId, Vec<Edge>>,
    inc: BTreeMap<BlockId, Vec<Edge>>,
    decision_status: BTreeMap<BlockId, DecisionStatus>,
    versions: BTreeMap<BlockId, Hash>,
}

const EMPTY: &[Edge] = &[];

pub fn same_content(a: &Block, b: &Block) -> bool {
    let strip = |x: &Block| {
        let mut x = x.clone();
        x.pins.clear();
        for f in ["status", "decided_at", "lifecycle"] {
            x.fields.remove(f);
        }
        x
    };
    strip(a) == strip(b)
}

impl Graph {
    pub fn build(blocks: &BTreeMap<BlockId, Block>) -> Graph {
        let mut g = Graph::default();
        for b in blocks.values() {
            g.versions.insert(b.id.clone(), b.version_hash());
            if let Some(s) = decision::status(b) {
                g.decision_status.insert(b.id.clone(), s);
            }
            let mut edges = vec![];
            if let Some(p) = &b.parent {
                edges.push(Edge { from: p.clone(), to: b.id.clone(), kind: EdgeKind::Contains, part: None, pin: None });
            }
            for tok in b.ref_tokens() {
                if let Ok(t) = BlockId::parse(&tok) {
                    let pin = b.pins.get(&t).copied();
                    edges.push(Edge { from: b.id.clone(), to: t, kind: EdgeKind::References, part: None, pin });
                }
            }
            for (name, links) in &b.links {
                for l in links {
                    edges.push(Edge { from: b.id.clone(), to: l.target.clone(), kind: EdgeKind::Link(name.clone()), part: l.part.clone(), pin: b.pins.get(&l.target).copied() });
                }
            }
            for e in edges {
                g.inc.entry(e.to.clone()).or_default().push(e.clone());
                g.out.entry(e.from.clone()).or_default().push(e);
            }
        }
        g
    }

    pub fn outgoing(&self, id: &BlockId) -> &[Edge] {
        self.out.get(id).map(Vec::as_slice).unwrap_or(EMPTY)
    }

    pub fn incoming(&self, id: &BlockId) -> &[Edge] {
        self.inc.get(id).map(Vec::as_slice).unwrap_or(EMPTY)
    }

    fn accepted(&self, id: &BlockId) -> bool {
        self.decision_status.get(id) == Some(&DecisionStatus::Accepted)
    }

    pub fn status(&self, id: &BlockId) -> Option<DerivedStatus> {
        match self.decision_status.get(id)? {
            DecisionStatus::Proposed => return Some(DerivedStatus::Proposed),
            DecisionStatus::Rejected => return Some(DerivedStatus::Rejected),
            DecisionStatus::Accepted => {}
        }
        let mut revoked = vec![];
        let mut whole = vec![];
        let mut partial = vec![];
        let mut parts = vec![];
        for e in self.incoming(id) {
            if !self.accepted(&e.from) {
                continue;
            }
            match &e.kind {
                EdgeKind::Link(n) if n == decision::REVOKES => revoked.push(e.from.clone()),
                EdgeKind::Link(n) if n == decision::SUPERSEDES => match &e.part {
                    None => whole.push(e.from.clone()),
                    Some(p) => {
                        partial.push(e.from.clone());
                        parts.push(p.clone());
                    }
                },
                _ => {}
            }
        }
        Some(if !revoked.is_empty() {
            DerivedStatus::Revoked { by: revoked }
        } else if !whole.is_empty() {
            DerivedStatus::Superseded { by: whole }
        } else if !partial.is_empty() {
            DerivedStatus::PartiallySuperseded { by: partial, parts }
        } else {
            DerivedStatus::Live
        })
    }

    fn is_live(&self, id: &BlockId) -> bool {
        matches!(self.status(id), Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. }))
    }

    pub fn governing(&self, id: &BlockId) -> Vec<BlockId> {
        let mut out = BTreeSet::new();
        for e in self.outgoing(id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::DECIDED_BY || n == decision::IMPLEMENTS) && self.is_live(&e.to) {
                out.insert(e.to.clone());
            }
        }
        for e in self.incoming(id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS) && self.is_live(&e.from) {
                out.insert(e.from.clone());
            }
        }
        out.into_iter().collect()
    }

    pub fn impact(&self, id: &BlockId, max_depth: u32) -> Vec<(u32, BlockId)> {
        let mut seen = BTreeSet::from([id.clone()]);
        let mut out = vec![];
        let mut queue = VecDeque::from([(0u32, id.clone())]);
        while let Some((d, cur)) = queue.pop_front() {
            if d == max_depth {
                continue;
            }
            for e in self.incoming(&cur) {
                if e.kind == EdgeKind::Contains || !seen.insert(e.from.clone()) {
                    continue;
                }
                out.push((d + 1, e.from.clone()));
                queue.push_back((d + 1, e.from.clone()));
            }
        }
        out.sort();
        out
    }

    pub fn suspects(&self, blocks: &BTreeMap<BlockId, Block>, old: &dyn Fn(&Hash) -> Option<Block>) -> Vec<Suspect> {
        let mut out = vec![];
        for edges in self.out.values() {
            for e in edges {
                if e.kind == EdgeKind::Contains {
                    continue;
                }
                // a superseding or revoking decision's own link is the cause, not a suspect
                if matches!(&e.kind, EdgeKind::Link(n) if n == decision::SUPERSEDES || n == decision::REVOKES) {
                    continue;
                }
                // a decision naming, in its text, an id it supersedes or revokes is expected
                if e.kind == EdgeKind::References
                    && self.outgoing(&e.from).iter().any(|o| o.to == e.to && matches!(&o.kind, EdgeKind::Link(n) if n == decision::SUPERSEDES || n == decision::REVOKES))
                {
                    continue;
                }
                let reason = match self.status(&e.to) {
                    Some(DerivedStatus::Superseded { .. }) => Some("superseded"),
                    Some(DerivedStatus::Revoked { .. }) => Some("revoked"),
                    _ => match (e.pin, self.versions.get(&e.to)) {
                        (Some(_), None) => Some("deleted"),
                        (Some(pin), Some(cur)) if pin != *cur => {
                            let same = match (old(&pin), blocks.get(&e.to)) {
                                (Some(a), Some(b)) => same_content(&a, b),
                                _ => false,
                            };
                            if same { None } else { Some("changed") }
                        }
                        _ => None,
                    },
                };
                if let Some(r) = reason {
                    out.push(Suspect { block: e.from.clone(), target: e.to.clone(), reason: r.into() });
                }
            }
        }
        out
    }
}
