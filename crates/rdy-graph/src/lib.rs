pub mod coord;

use imbl::OrdMap;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{Block, BlockId, Hash, Tracking};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum EdgeKind {
    Contains,
    References,
    Link(String),
}

impl EdgeKind {
    /// "contains", "references", or the link's name.
    pub fn name(&self) -> String {
        match self {
            EdgeKind::Contains => "contains".into(),
            EdgeKind::References => "references".into(),
            EdgeKind::Link(n) => n.clone(),
        }
    }
}

/// imp1: the edge that first reached an impact item. `from` is the block the item depends on
/// (the queried block, or an item nearer to it); `kind` is that edge's kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Via {
    pub from: BlockId,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImpactHit {
    pub depth: u32,
    pub id: BlockId,
    pub via: Via,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Edge {
    pub from: BlockId,
    pub to: BlockId,
    pub kind: EdgeKind,
    pub part: Option<String>,
    /// pin1: printed as hex like every other hash in rdy's JSON. Only the output changes;
    /// `Hash`'s own (postcard) serialization, which hashes depend on, does not.
    #[serde(serialize_with = "pin_hex")]
    pub pin: Option<Hash>,
}

fn pin_hex<S: serde::Serializer>(pin: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match pin {
        Some(h) => s.serialize_some(&h.to_string()),
        None => s.serialize_none(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DerivedStatus {
    Proposed,
    Rejected,
    Live,
    PartiallySuperseded {
        by: Vec<BlockId>,
        parts: Vec<String>,
    },
    Superseded {
        by: Vec<BlockId>,
    },
    Revoked {
        by: Vec<BlockId>,
    },
}

impl DerivedStatus {
    /// The status's name, as its serialized `status` tag spells it.
    pub fn tag(&self) -> &'static str {
        match self {
            DerivedStatus::Proposed => "proposed",
            DerivedStatus::Rejected => "rejected",
            DerivedStatus::Live => "live",
            DerivedStatus::PartiallySuperseded { .. } => "partially_superseded",
            DerivedStatus::Superseded { .. } => "superseded",
            DerivedStatus::Revoked { .. } => "revoked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Suspect {
    pub block: BlockId,
    pub target: BlockId,
    pub reason: String,
}

/// The edges, content hashes and decision statuses of one state. Its maps are persistent, so a clone is
/// O(1) and [`Graph::with_changes`] carries a graph across a write in O(changed edges × log n).
/// Every edge list is ordered by (generator id, index within the generator's edges), where an
/// edge's generator is the block whose own content makes it (see `Graph::edges_of`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    out: OrdMap<BlockId, Vec<Edge>>,
    inc: OrdMap<BlockId, Vec<Edge>>,
    decision_status: OrdMap<BlockId, DecisionStatus>,
    /// lpin1: each block's [`Block::content_hash_with`] the graph's tracking fields, which a pin
    /// is checked against.
    contents: OrdMap<BlockId, Hash>,
    /// trk1: the project pack's tracking fields, left out of every content hash.
    tracking: Arc<Tracking>,
}

const EMPTY: &[Edge] = &[];

/// Whether `a` and `b` differ only in their pins and derived fields.
pub fn same_content(a: &Block, b: &Block) -> bool {
    a.content_hash() == b.content_hash()
}

/// How strong a suspect's reason is: revoked, then superseded, then deleted, then changed.
fn rank(reason: &str) -> u8 {
    match reason {
        "revoked" => 4,
        "superseded" => 3,
        "deleted" => 2,
        _ => 1,
    }
}

/// sus1: one suspect per (block, target), keeping the strongest reason, so a block that both
/// references and links to a target counts once.
fn unique(found: impl Iterator<Item = Suspect>) -> Vec<Suspect> {
    let mut best: BTreeMap<(BlockId, BlockId), Suspect> = BTreeMap::new();
    for s in found {
        let key = (s.block.clone(), s.target.clone());
        match best.get(&key) {
            Some(b) if rank(&b.reason) >= rank(&s.reason) => {}
            _ => {
                best.insert(key, s);
            }
        }
    }
    best.into_values().collect()
}

/// sus2: link kinds through which a changed or deleted target makes a block suspect: premises,
/// and what the block implements or is decided by (spec §6.2: "references and premises").
/// A superseded or revoked target is suspect through any link.
pub const SUSPECT_LINKS: [&str; 4] = [
    decision::DEPENDS_ON,
    decision::CONDITIONAL_ON,
    decision::IMPLEMENTS,
    decision::DECIDED_BY,
];

/// imp2: link kinds impact follows: the dependency edges (with references).
pub const IMPACT_LINKS: [&str; 6] = [
    decision::DEPENDS_ON,
    decision::CONDITIONAL_ON,
    decision::IMPLEMENTS,
    decision::DECIDED_BY,
    decision::DESCRIBES,
    decision::AFFECTS,
];
/// imp2: link kinds impact lists at depth 1 (decisions that refine or supersede the queried
/// block) without following them.
pub const IMPACT_SHOWN: [&str; 2] = [decision::REFINES, decision::SUPERSEDES];

/// The block whose own content makes `e`: the child for `Contains`, `from` otherwise.
fn generator(e: &Edge) -> &BlockId {
    match e.kind {
        EdgeKind::Contains => &e.to,
        _ => &e.from,
    }
}

/// Removes the first element equal to `e` from `map[key]`, dropping the list once it is empty.
fn unlist(map: &mut OrdMap<BlockId, Vec<Edge>>, key: &BlockId, e: &Edge) {
    let Some(list) = map.get_mut(key) else {
        return;
    };
    let start = list.partition_point(|x| generator(x) < generator(e));
    if let Some(i) = list[start..].iter().position(|x| x == e) {
        list.remove(start + i);
    }
    if list.is_empty() {
        map.remove(key);
    }
}

/// Inserts `e` into `map[key]` after every edge whose generator sorts at or before `e`'s, so a
/// generator's edges, inserted in order, stay in its run and in their own order.
fn enlist(map: &mut OrdMap<BlockId, Vec<Edge>>, key: BlockId, e: Edge) {
    let list = map.entry(key).or_default();
    let at = list.partition_point(|x| generator(x) <= generator(&e));
    list.insert(at, e);
}

/// The edges from `b` that its own content makes, in the order a graph lists them: one
/// `References` edge per reference token in set order, then its links in map order, each with
/// its pin (and a link with its part). The `Contains` edge from `b`'s parent is not one of them.
pub fn edges_of(b: &Block) -> Vec<Edge> {
    let mut edges = vec![];
    for tok in b.ref_tokens() {
        if let Ok(t) = BlockId::parse(&tok) {
            let pin = b.pins.get(&t).copied();
            edges.push(Edge {
                from: b.id.clone(),
                to: t,
                kind: EdgeKind::References,
                part: None,
                pin,
            });
        }
    }
    for (name, links) in &b.links {
        for l in links {
            edges.push(Edge {
                from: b.id.clone(),
                to: l.target.clone(),
                kind: EdgeKind::Link(name.clone()),
                part: l.part.clone(),
                pin: b.pins.get(&l.target).copied(),
            });
        }
    }
    edges
}

impl Graph {
    /// The graph of `blocks`, which are visited in the order given (id order, for a state), with
    /// no tracking fields.
    pub fn build_from<'a>(blocks: impl IntoIterator<Item = &'a Block>) -> Graph {
        Graph::build_with(blocks, Arc::default())
    }

    /// trk1: the graph of `blocks` whose content hashes leave out `tracking`'s fields.
    pub fn build_with<'a>(
        blocks: impl IntoIterator<Item = &'a Block>,
        tracking: Arc<Tracking>,
    ) -> Graph {
        let mut g = Graph {
            tracking,
            ..Graph::default()
        };
        for b in blocks {
            g.contents
                .insert(b.id.clone(), b.content_hash_with(&g.tracking));
            if let Some(s) = decision::status(b) {
                g.decision_status.insert(b.id.clone(), s);
            }
            for e in Graph::edges_of(b) {
                g.inc.entry(e.to.clone()).or_default().push(e.clone());
                g.out.entry(e.from.clone()).or_default().push(e);
            }
        }
        g
    }

    /// The edges `b`'s own content makes, in order: `Contains` from its parent (if any), then
    /// [`edges_of`]'s.
    fn edges_of(b: &Block) -> Vec<Edge> {
        let mut edges = vec![];
        if let Some(p) = &b.parent {
            edges.push(Edge {
                from: p.clone(),
                to: b.id.clone(),
                kind: EdgeKind::Contains,
                part: None,
                pin: None,
            });
        }
        edges.extend(edges_of(b));
        edges
    }

    /// gcar1: `self` with every edge, content hash and status the `old` blocks generate removed and
    /// those of the `new` blocks added. With `old` the previous versions of a write's blocks and
    /// `new` their current versions, the result equals a rebuild of the new state.
    pub fn with_changes<'a>(
        &self,
        old: impl IntoIterator<Item = &'a Block>,
        new: impl IntoIterator<Item = &'a Block>,
    ) -> Graph {
        let mut g = self.clone();
        for b in old {
            g.contents.remove(&b.id);
            g.decision_status.remove(&b.id);
            for e in Graph::edges_of(b) {
                unlist(&mut g.out, &e.from, &e);
                unlist(&mut g.inc, &e.to, &e);
            }
        }
        for b in new {
            g.contents
                .insert(b.id.clone(), b.content_hash_with(&g.tracking));
            if let Some(s) = decision::status(b) {
                g.decision_status.insert(b.id.clone(), s);
            }
            for e in Graph::edges_of(b) {
                enlist(&mut g.inc, e.to.clone(), e.clone());
                enlist(&mut g.out, e.from.clone(), e);
            }
        }
        g
    }

    /// trk1: the tracking fields this graph's content hashes leave out.
    pub fn tracking(&self) -> &Arc<Tracking> {
        &self.tracking
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
        matches!(
            self.status(id),
            Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
        )
    }

    pub fn governing(&self, id: &BlockId) -> Vec<BlockId> {
        let mut out = BTreeSet::new();
        for e in self.outgoing(id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::DECIDED_BY || n == decision::IMPLEMENTS)
                && self.is_live(&e.to)
            {
                out.insert(e.to.clone());
            }
        }
        for e in self.incoming(id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS)
                && self.is_live(&e.from)
            {
                out.insert(e.from.clone());
            }
        }
        out.into_iter().collect()
    }

    /// FW-LP6: the blocks decision `d` governs, whatever its status: each block with a
    /// `decided_by` or `implements` link to `d`, and each block `d` affects. By id, once each.
    pub fn governed_by(&self, d: &BlockId) -> Vec<BlockId> {
        let mut out = BTreeSet::new();
        for e in self.incoming(d) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::DECIDED_BY || n == decision::IMPLEMENTS)
            {
                out.insert(e.from.clone());
            }
        }
        for e in self.outgoing(d) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS) {
                out.insert(e.to.clone());
            }
        }
        out.into_iter().collect()
    }

    pub fn impact(&self, id: &BlockId, max_depth: u32) -> Vec<(u32, BlockId)> {
        self.impact_via(id, max_depth)
            .into_iter()
            .map(|h| (h.depth, h.id))
            .collect()
    }

    /// Everything that depends on `id`, breadth first up to `max_depth`, each with the edge that
    /// first reached it; sorted by (depth, id). Follows references and dependency links
    /// (IMPACT_LINKS); decisions that refine or supersede `id` itself are listed at depth 1 and
    /// not followed (IMPACT_SHOWN).
    pub fn impact_via(&self, id: &BlockId, max_depth: u32) -> Vec<ImpactHit> {
        // value: whether the block's own dependents are followed
        let mut seen: BTreeMap<BlockId, bool> = BTreeMap::from([(id.clone(), true)]);
        let mut out = vec![];
        let mut queue = VecDeque::from([(0u32, id.clone())]);
        while let Some((d, cur)) = queue.pop_front() {
            if d == max_depth {
                continue;
            }
            for e in self.incoming(&cur) {
                let (follow, show) = match &e.kind {
                    EdgeKind::Contains => (false, false),
                    EdgeKind::References => (true, true),
                    EdgeKind::Link(n) if IMPACT_LINKS.contains(&n.as_str()) => (true, true),
                    EdgeKind::Link(n) if IMPACT_SHOWN.contains(&n.as_str()) => (false, cur == *id),
                    EdgeKind::Link(_) => (false, false),
                };
                if !show {
                    continue;
                }
                match seen.get(&e.from) {
                    Some(true) => {}
                    Some(false) if !follow => {}
                    Some(false) => {
                        // listed through a refine or supersede; a dependency edge also reaches it
                        seen.insert(e.from.clone(), true);
                        queue.push_back((d + 1, e.from.clone()));
                    }
                    None => {
                        seen.insert(e.from.clone(), follow);
                        out.push(ImpactHit {
                            depth: d + 1,
                            id: e.from.clone(),
                            via: Via {
                                from: cur.clone(),
                                kind: e.kind.name(),
                            },
                        });
                        if follow {
                            queue.push_back((d + 1, e.from.clone()));
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| (a.depth, &a.id).cmp(&(b.depth, &b.id)));
        out
    }

    pub fn suspects(&self) -> Vec<Suspect> {
        unique(
            self.out
                .values()
                .flatten()
                .filter_map(|e| self.edge_suspect(e)),
        )
    }

    /// The suspects whose `block` is `id`: checks only that block's own outgoing edges.
    pub fn suspects_of(&self, id: &BlockId) -> Vec<Suspect> {
        unique(
            self.outgoing(id)
                .iter()
                .filter_map(|e| self.edge_suspect(e)),
        )
    }

    /// lpin1: a pinned premise edge is `deleted` when its target is gone and `changed` when the
    /// pin differs from the target's content hash. A block is never a suspect of itself: a
    /// write never pins a block to its own id, and a stray self-pin is ignored here.
    fn edge_suspect(&self, e: &Edge) -> Option<Suspect> {
        if e.kind == EdgeKind::Contains || e.from == e.to {
            return None;
        }
        // a superseding or revoking decision's own link is the cause, not a suspect
        if matches!(&e.kind, EdgeKind::Link(n) if n == decision::SUPERSEDES || n == decision::REVOKES)
        {
            return None;
        }
        // a decision naming, in its text, an id it supersedes or revokes is expected
        if e.kind == EdgeKind::References
            && self.outgoing(&e.from).iter().any(|o| o.to == e.to && matches!(&o.kind, EdgeKind::Link(n) if n == decision::SUPERSEDES || n == decision::REVOKES))
        {
            return None;
        }
        let reason = match self.status(&e.to) {
            Some(DerivedStatus::Superseded { .. }) => "superseded",
            Some(DerivedStatus::Revoked { .. }) => "revoked",
            _ => {
                let premise = match &e.kind {
                    EdgeKind::References => true,
                    EdgeKind::Link(n) => SUSPECT_LINKS.contains(&n.as_str()),
                    EdgeKind::Contains => false,
                };
                if !premise {
                    return None;
                }
                match (e.pin, self.contents.get(&e.to)) {
                    (Some(_), None) => "deleted",
                    (Some(pin), Some(cur)) if pin != *cur => "changed",
                    _ => return None,
                }
            }
        };
        Some(Suspect {
            block: e.from.clone(),
            target: e.to.clone(),
            reason: reason.into(),
        })
    }
}
