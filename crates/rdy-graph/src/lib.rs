pub mod coord;
#[cfg(test)]
mod relink_tests;

use imbl::OrdMap;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::engine_fields::{self, Against};
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
/// O(1).
/// Relink work is linear in E_old + E_new + U for the whole call, plus one stable sort per changed key. The persistent maps copy each touched leaf once per call, with every list in that leaf; this copy is not counted here.
/// Every edge list is ordered by (generator id, index within the generator's edges), where an
/// edge's generator is the block whose own content makes it (see `Graph::edges_of`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    out: OrdMap<BlockId, Vec<Edge>>,
    inc: OrdMap<BlockId, Vec<Edge>>,
    decision_status: OrdMap<BlockId, DecisionStatus>,
    /// lpin1: each block's [`Block::content_hash_with`] the graph's tracking fields, which a pin
    /// is checked against. A removed block's stub is never here: its tracking hash cannot be
    /// recomputed (D-3M81W).
    contents: OrdMap<BlockId, Hash>,
    /// Each removed block (a read projection, [`Block::is_redacted`]) and the request that removed
    /// it, apart from `contents`.
    redacted: OrdMap<BlockId, Hash>,
    /// p-QMGB4 (D-EJBPD): each decision's decoded `reaffirmed` record, by target: the
    /// retirement or deletion it re-affirmed that premise against. Only decisions with a valid,
    /// non-empty record are here. [`Graph::edge_suspect`] alone reads it.
    reaffirmed: OrdMap<BlockId, BTreeMap<BlockId, Against>>,
    /// trk1: the project pack's tracking fields, left out of every content hash.
    tracking: Arc<Tracking>,
}

const EMPTY: &[Edge] = &[];

/// The `via` of a route through an entity the decision affects and the block describes.
const DESCRIBES_AFFECTS: &str = "describes/affects";

/// rf3: which decision statuses a governing query admits. `Live` includes a partly superseded
/// decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoverningStatuses {
    Live,
    LiveOrProposed,
    All,
}

/// rf3: one decision that governs a block and the route that reaches it. `via` is exactly
/// `decided_by`, `implements`, `affects` or `describes/affects`; `entity` is the entity of the
/// last route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoverningRoute {
    pub decision: BlockId,
    pub via: &'static str,
    pub entity: Option<BlockId>,
}

/// rf3: the decisions with an incoming `affects` edge, by entity, so a read scans each entity
/// once however many blocks describe it. The candidates are unfiltered, so one cache serves
/// every status filter. A cache belongs to one [`Graph`] and one read: never reuse it after the
/// graph changes.
#[derive(Debug, Default)]
pub struct GoverningCache {
    entities: BTreeMap<BlockId, Vec<BlockId>>,
    scans: usize,
}

impl GoverningCache {
    /// How many entities this cache has scanned. Test support only.
    #[doc(hidden)]
    pub fn entity_scans(&self) -> usize {
        self.scans
    }

    /// The sorted ids of the decisions that affect `entity`, scanned once.
    fn entity_decisions(&mut self, g: &Graph, entity: &BlockId) -> &[BlockId] {
        if !self.entities.contains_key(entity) {
            self.scans += 1;
            let ids: BTreeSet<BlockId> = g
                .incoming(entity)
                .iter()
                .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS))
                .filter(|e| g.status(&e.from).is_some())
                .map(|e| e.from.clone())
                .collect();
            self.entities
                .insert(entity.clone(), ids.into_iter().collect());
        }
        &self.entities[entity]
    }
}

/// The key of one difference between two maps.
fn key_of<'a, V>(item: imbl::ordmap::DiffItem<'a, 'a, BlockId, V>) -> &'a BlockId {
    match item {
        imbl::ordmap::DiffItem::Add(k, _) | imbl::ordmap::DiffItem::Remove(k, _) => k,
        imbl::ordmap::DiffItem::Update { old, .. } => old.0,
    }
}

/// Whether `a` and `b` differ only in their pins and derived fields. When either is a removed
/// block's projection, its content is unknown: the two are the same only when their recorded
/// versions are.
pub fn same_content(a: &Block, b: &Block) -> bool {
    if a.is_redacted() || b.is_redacted() {
        return a.version_hash() == b.version_hash();
    }
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
///
/// rf3 (D-4WC6R): `reflects` joins them. A block that reflects a source is suspect when that
/// source changes or is deleted, with the pin the link already carries.
pub const SUSPECT_LINKS: [&str; 5] = [
    decision::DEPENDS_ON,
    decision::CONDITIONAL_ON,
    decision::IMPLEMENTS,
    decision::DECIDED_BY,
    REFLECTS,
];

/// imp2: link kinds impact follows: the dependency edges (with references).
pub const IMPACT_LINKS: [&str; 7] = [
    decision::DEPENDS_ON,
    decision::CONDITIONAL_ON,
    decision::IMPLEMENTS,
    decision::DECIDED_BY,
    decision::DESCRIBES,
    decision::AFFECTS,
    REFLECTS,
];

/// The engine link kind from a block to a source it reflects. Only the graph's suspect and impact
/// sets name it; lineage and answer links do not.
const REFLECTS: &str = "reflects";
/// imp2: link kinds impact lists at depth 1 (decisions that refine or supersede the queried
/// block) without following them.
pub const IMPACT_SHOWN: [&str; 2] = [decision::REFINES, decision::SUPERSEDES];

#[cfg(test)]
thread_local! {
    /// The edge work [`Graph::with_changes`] has done on this thread: comparisons, set lookups,
    /// edges copied, slots visited and slots moved in the adjacency lists the call changes. It
    /// does not count the copy-on-write copies the persistent maps make. Test support only;
    /// `relink_tests` resets and reads it.
    static WORK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Charges `n` units of edge work to this thread's test counter. Outside tests it does nothing.
#[inline(always)]
fn charge(n: usize) {
    #[cfg(test)]
    WORK.with(|w| w.set(w.get() + n as u64));
    #[cfg(not(test))]
    let _ = n;
}

/// Resets this thread's work counter to zero.
#[cfg(test)]
fn reset_work() {
    WORK.with(|w| w.set(0));
}

/// This thread's work since the last [`reset_work`].
#[cfg(test)]
fn work() -> u64 {
    WORK.with(|w| w.get())
}

/// The block whose own content makes `e`: the child for `Contains`, `from` otherwise.
fn generator(e: &Edge) -> &BlockId {
    match e.kind {
        EdgeKind::Contains => &e.to,
        _ => &e.from,
    }
}

/// Removes from each list in `keys` every edge an `old` block generates, in one stable pass per
/// list, and drops a list once it is empty.
fn drop_generated(
    map: &mut OrdMap<BlockId, Vec<Edge>>,
    keys: &BTreeSet<BlockId>,
    old: &BTreeSet<BlockId>,
) {
    for key in keys {
        charge(1);
        let Some(list) = map.get_mut(key) else {
            continue;
        };
        list.retain(|e| {
            // the slot visited and the set lookup
            charge(2);
            !old.contains(generator(e))
        });
        // each kept slot is moved at most once
        charge(list.len());
        if list.is_empty() {
            map.remove(key);
        }
    }
}

/// Merges each group of new edges into its list in one pass. A group is stably sorted by
/// generator first, so a generator's edges keep their own order; at equal generators every
/// existing edge comes before a new one. Edges are never deduplicated.
fn merge_generated(map: &mut OrdMap<BlockId, Vec<Edge>>, groups: BTreeMap<BlockId, Vec<Edge>>) {
    for (key, mut add) in groups {
        add.sort_by(|a, b| {
            charge(1);
            generator(a).cmp(generator(b))
        });
        charge(1);
        let list = map.entry(key).or_default();
        let kept = std::mem::take(list);
        let mut merged = Vec::with_capacity(kept.len() + add.len());
        let mut add = add.into_iter().peekable();
        for e in kept {
            while let Some(n) = add.next_if(|n| {
                charge(1);
                generator(n) < generator(&e)
            }) {
                charge(1);
                merged.push(n);
            }
            charge(1);
            merged.push(e);
        }
        charge(add.len());
        merged.extend(add);
        *list = merged;
    }
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

/// `b`'s `reaffirmed` record, by target, when it holds one with a valid shape and at least one
/// target. Read validation refuses a malformed record; one here adds no exemption.
fn reaffirmed_of(b: &Block) -> Option<BTreeMap<BlockId, Against>> {
    let v = b.fields.get(engine_fields::REAFFIRMED)?;
    let acks: BTreeMap<BlockId, Against> = engine_fields::decode_reaffirmed(v)
        .ok()?
        .into_iter()
        .map(|(t, r)| (t, r.against))
        .collect();
    (!acks.is_empty()).then_some(acks)
}

/// p-QMGB4 (task-5 R1): whether `b`'s reference to the absent `target` is a kept citation, not a
/// dangling one: `b` is an accepted decision whose `reaffirmed` record holds `deleted` for
/// exactly `target`. The write, the proposal check and the landing check all ask this one
/// question of the version they check. A restored target is not absent, so the acknowledgement
/// ends with its return, as the graph's suspects already treat it.
pub fn keeps_deleted_citation(b: &Block, target: &BlockId) -> bool {
    decision::status(b) == Some(DecisionStatus::Accepted)
        && reaffirmed_of(b).is_some_and(|acks| matches!(acks.get(target), Some(Against::Deleted)))
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
            g.add_content(b);
            g.add_status(b);
            for e in Graph::edges_of(b) {
                g.inc.entry(e.to.clone()).or_default().push(e.clone());
                g.out.entry(e.from.clone()).or_default().push(e);
            }
        }
        g
    }

    /// Records what a pin to `b` is checked against: its tracking-sensitive content hash, or,
    /// for a removed block's stub, only the request that removed it.
    fn add_content(&mut self, b: &Block) {
        match b.redaction() {
            Some(request) => {
                self.redacted.insert(b.id.clone(), request);
            }
            None => {
                self.contents
                    .insert(b.id.clone(), b.content_hash_with(&self.tracking));
            }
        }
    }

    /// Records `b`'s decision status and, for a decision, its `reaffirmed` record.
    fn add_status(&mut self, b: &Block) {
        let Some(s) = decision::status(b) else {
            return;
        };
        self.decision_status.insert(b.id.clone(), s);
        if let Some(acks) = reaffirmed_of(b) {
            self.reaffirmed.insert(b.id.clone(), acks);
        }
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
        let mut old_ids = BTreeSet::new();
        let mut outs = BTreeSet::new();
        let mut incs = BTreeSet::new();
        for b in old {
            g.contents.remove(&b.id);
            g.redacted.remove(&b.id);
            g.decision_status.remove(&b.id);
            g.reaffirmed.remove(&b.id);
            charge(1);
            old_ids.insert(b.id.clone());
            for e in Graph::edges_of(b) {
                charge(2);
                outs.insert(e.from);
                incs.insert(e.to);
            }
        }
        drop_generated(&mut g.out, &outs, &old_ids);
        drop_generated(&mut g.inc, &incs, &old_ids);
        let mut add_out: BTreeMap<BlockId, Vec<Edge>> = BTreeMap::new();
        let mut add_inc: BTreeMap<BlockId, Vec<Edge>> = BTreeMap::new();
        for b in new {
            g.add_content(b);
            g.add_status(b);
            for e in Graph::edges_of(b) {
                // two group lookups and two copies
                charge(4);
                add_inc.entry(e.to.clone()).or_default().push(e.clone());
                add_out.entry(e.from.clone()).or_default().push(e);
            }
        }
        merge_generated(&mut g.out, add_out);
        merge_generated(&mut g.inc, add_inc);
        g
    }

    /// pub2 (D-0VCQ5): the graph of the blocks in `keep` alone. An edge stays only when both its
    /// endpoints are in `keep`, in the order this graph lists it, with its pin and part. Content
    /// hashes, decision statuses and removal records stay only for blocks in `keep`; the tracking
    /// fields are the same. An id in `keep` that this graph does not know adds nothing, and
    /// `self` is unchanged. Queries on the result (status, suspects, impact, governing, removed
    /// pins) can never reach a block outside `keep`.
    pub fn induced(&self, keep: &BTreeSet<BlockId>) -> Graph {
        fn edges(
            map: &OrdMap<BlockId, Vec<Edge>>,
            keep: &BTreeSet<BlockId>,
        ) -> OrdMap<BlockId, Vec<Edge>> {
            keep.iter()
                .filter_map(|id| {
                    let list: Vec<Edge> = map
                        .get(id)?
                        .iter()
                        .filter(|e| keep.contains(&e.from) && keep.contains(&e.to))
                        .cloned()
                        .collect();
                    (!list.is_empty()).then(|| (id.clone(), list))
                })
                .collect()
        }
        fn values<V: Clone>(
            map: &OrdMap<BlockId, V>,
            keep: &BTreeSet<BlockId>,
        ) -> OrdMap<BlockId, V> {
            keep.iter()
                .filter_map(|id| Some((id.clone(), map.get(id)?.clone())))
                .collect()
        }
        Graph {
            out: edges(&self.out, keep),
            inc: edges(&self.inc, keep),
            decision_status: values(&self.decision_status, keep),
            contents: values(&self.contents, keep),
            redacted: values(&self.redacted, keep),
            reaffirmed: values(&self.reaffirmed, keep),
            tracking: self.tracking.clone(),
        }
    }

    /// The removed targets `id` holds a pin to, each once, sorted by target id, with the request
    /// that removed it. The pin keeps its value; a removed target is never a `changed` or
    /// `deleted` suspect (D-3M81W).
    pub fn redacted_pins_of(&self, id: &BlockId) -> Vec<(BlockId, Hash)> {
        let found: BTreeMap<&BlockId, Hash> = self
            .outgoing(id)
            .iter()
            .filter(|e| e.pin.is_some() && e.from != e.to)
            .filter_map(|e| Some((&e.to, *self.redacted.get(&e.to)?)))
            .collect();
        found.into_iter().map(|(t, r)| (t.clone(), r)).collect()
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

    /// p-HF5W0: the accepted decisions that supersede (wholly or in part) or revoke `id`,
    /// sorted and once each: the set a re-affirmation of a premise on `id` records. It holds
    /// every partial superseder, also when a whole superseder or a revoker sets the status.
    pub fn retiring_decisions(&self, id: &BlockId) -> BTreeSet<BlockId> {
        self.incoming(id)
            .iter()
            .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == decision::SUPERSEDES || n == decision::REVOKES))
            .filter(|e| self.accepted(&e.from))
            .map(|e| e.from.clone())
            .collect()
    }

    /// p-QMGB4: what `source` re-affirmed its premise on `target` against, when `source` is an
    /// accepted decision that is live or partly superseded. A proposed, rejected, wholly
    /// superseded or revoked decision, or a block that is no decision, has no exemption.
    fn reaffirmed_against(&self, source: &BlockId, target: &BlockId) -> Option<&Against> {
        let against = self.reaffirmed.get(source)?.get(target)?;
        self.is_live(source).then_some(against)
    }

    /// Whether `source` re-affirmed its premise on the retired `target` against exactly the
    /// decisions that retire it now. A decision added to or taken from that set breaks it.
    fn retirement_reaffirmed(&self, source: &BlockId, target: &BlockId) -> bool {
        match self.reaffirmed_against(source, target) {
            Some(Against::Retiring(ids)) => ids.iter().eq(self.retiring_decisions(target).iter()),
            _ => false,
        }
    }

    fn is_live(&self, id: &BlockId) -> bool {
        matches!(
            self.status(id),
            Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
        )
    }

    /// rf3 (D-VPATZ): the decisions that govern `id`, each once, by decision id, with the one
    /// route that reaches it. The routes in priority order are `decided_by`, `implements`,
    /// `affects` (the decision affects `id`) and `describes/affects` (`id` describes an entity,
    /// per `is_entity`, that the decision affects; the lowest entity id wins a tie). References,
    /// text mentions, an entity's home and a describes target that is no entity add no route;
    /// a target without a status is no decision. `statuses` filters the decisions. `cache`
    /// belongs to this graph and this read.
    pub fn governing_routes(
        &self,
        id: &BlockId,
        statuses: GoverningStatuses,
        is_entity: impl Fn(&BlockId) -> bool,
        cache: &mut GoverningCache,
    ) -> Vec<GoverningRoute> {
        let mut found: BTreeMap<BlockId, (u8, GoverningRoute)> = BTreeMap::new();
        let mut offer = |rank: u8, d: &BlockId, via: &'static str, entity: Option<&BlockId>| {
            let route = GoverningRoute {
                decision: d.clone(),
                via,
                entity: entity.cloned(),
            };
            match found.get(d) {
                // a lower rank wins; within describes/affects the lower entity id wins
                Some((r, old)) if (*r, &old.entity) <= (rank, &route.entity) => {}
                _ => {
                    found.insert(d.clone(), (rank, route));
                }
            }
        };
        for e in self.outgoing(id) {
            let EdgeKind::Link(n) = &e.kind else { continue };
            match n.as_str() {
                decision::DECIDED_BY if self.governs_in(&e.to, statuses) => {
                    offer(0, &e.to, decision::DECIDED_BY, None);
                }
                decision::IMPLEMENTS if self.governs_in(&e.to, statuses) => {
                    offer(1, &e.to, decision::IMPLEMENTS, None);
                }
                decision::DESCRIBES if is_entity(&e.to) => {
                    for d in cache.entity_decisions(self, &e.to) {
                        if self.governs_in(d, statuses) {
                            offer(3, d, DESCRIBES_AFFECTS, Some(&e.to));
                        }
                    }
                }
                _ => {}
            }
        }
        for e in self.incoming(id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS)
                && self.governs_in(&e.from, statuses)
            {
                offer(2, &e.from, decision::AFFECTS, None);
            }
        }
        found.into_values().map(|(_, r)| r).collect()
    }

    /// rf3: the inverse of [`Graph::governing_routes`]: the blocks decision `d` governs, by id,
    /// once each. The direct set ([`Graph::governed_by`]) plus each block that describes an
    /// entity `d` affects. A missing `d`, a block that is no decision, or a status `statuses`
    /// excludes gives none. It follows the same route rule and cache as `governing_routes`.
    pub fn governed_through(
        &self,
        d: &BlockId,
        statuses: GoverningStatuses,
        is_entity: impl Fn(&BlockId) -> bool,
        cache: &mut GoverningCache,
    ) -> Vec<BlockId> {
        if !self.governs_in(d, statuses) {
            return vec![];
        }
        let mut out: BTreeSet<BlockId> = self.governed_by(d).into_iter().collect();
        for e in self.outgoing(d) {
            if !matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS)
                || !is_entity(&e.to)
                || !cache.entity_decisions(self, &e.to).contains(d)
            {
                continue;
            }
            for r in self.incoming(&e.to) {
                if matches!(&r.kind, EdgeKind::Link(n) if n == decision::DESCRIBES) {
                    out.insert(r.from.clone());
                }
            }
        }
        out.into_iter().collect()
    }

    /// Whether `id` is a decision whose derived status `statuses` admits.
    fn governs_in(&self, id: &BlockId, statuses: GoverningStatuses) -> bool {
        match self.status(id) {
            None => false,
            Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. }) => true,
            Some(DerivedStatus::Proposed) => statuses != GoverningStatuses::Live,
            Some(_) => statuses == GoverningStatuses::All,
        }
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

    /// perf4: the blocks whose suspects can differ between `prev` and this graph; every other
    /// block's suspects (`suspects_of`) are the same in both. A suspect of block X on target T
    /// reads X's outgoing edges, T's content hash and T's status, which reads T's decision status
    /// and its incoming edges from decisions with theirs. So: each block whose outgoing edges
    /// differ, and each block with an edge (in either graph) to a target whose content hash,
    /// decision status or incoming edges differ, or to which a decision whose status differs
    /// has an edge; and each decision whose `reaffirmed` record differs, or that has one and
    /// whose own status can differ. Found by diffing the graphs' maps, which skips the
    /// structure a graph carried across a write (`with_changes`) shares with the one it came
    /// from: the cost follows what changed, not the size of the graph.
    pub fn suspect_candidates(&self, prev: &Graph) -> BTreeSet<BlockId> {
        let mut out: BTreeSet<BlockId> = BTreeSet::new();
        let mut targets: BTreeSet<BlockId> = BTreeSet::new();
        for item in prev.out.diff(&self.out) {
            // (an edge that came or went is in its target's incoming edges too)
            out.insert(key_of(item).clone());
        }
        for item in prev.inc.diff(&self.inc) {
            targets.insert(key_of(item).clone());
        }
        for item in prev.contents.diff(&self.contents) {
            targets.insert(key_of(item).clone());
        }
        for item in prev.redacted.diff(&self.redacted) {
            targets.insert(key_of(item).clone());
        }
        for item in prev.decision_status.diff(&self.decision_status) {
            let d = key_of(item);
            targets.insert(d.clone());
            // its status decides whether its links move its targets' status
            for g in [prev, self] {
                targets.extend(g.outgoing(d).iter().map(|e| e.to.clone()));
            }
        }
        // p-QMGB4: a decision's `reaffirmed` record decides its own suspects, and so does its
        // status (only a live decision uses the record). Every block whose status can differ is
        // in `targets`.
        for item in prev.reaffirmed.diff(&self.reaffirmed) {
            out.insert(key_of(item).clone());
        }
        for t in &targets {
            for g in [prev, self] {
                out.extend(g.incoming(t).iter().map(|e| e.from.clone()));
                if g.reaffirmed.contains_key(t) {
                    out.insert(t.clone());
                }
            }
        }
        out
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
    /// pin differs from the target's content hash. A removed target (a stub) is neither: its
    /// content is unavailable, and [`Graph::redacted_pins_of`] names it (D-3M81W). A block is
    /// never a suspect of itself: a write never pins a block to its own id, and a stray self-pin
    /// is ignored here. Public so a consequence names the edge that made a suspect from this one
    /// rule (df4).
    pub fn edge_suspect(&self, e: &Edge) -> Option<Suspect> {
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
        let retired = match self.status(&e.to) {
            Some(DerivedStatus::Superseded { .. }) => Some("superseded"),
            Some(DerivedStatus::Revoked { .. }) => Some("revoked"),
            _ => None,
        };
        // p-QMGB4: a retirement re-affirmed against its whole current set is no cause; the
        // premise's pin still decides whether its content drifted
        let reason = match retired {
            Some(r) if !self.retirement_reaffirmed(&e.from, &e.to) => r,
            _ => {
                let premise = match &e.kind {
                    EdgeKind::References => true,
                    EdgeKind::Link(n) => SUSPECT_LINKS.contains(&n.as_str()),
                    EdgeKind::Contains => false,
                };
                if !premise || self.redacted.contains_key(&e.to) {
                    return None;
                }
                match (e.pin, self.contents.get(&e.to)) {
                    // p-HF5W0: `against: deleted` holds while the target stays absent
                    (Some(_), None)
                        if matches!(
                            self.reaffirmed_against(&e.from, &e.to),
                            Some(Against::Deleted)
                        ) =>
                    {
                        return None;
                    }
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
