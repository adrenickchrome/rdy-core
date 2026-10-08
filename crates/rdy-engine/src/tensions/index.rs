//! The shared index the detectors read: the in-flight writes (with their liveness and their name,
//! text, supersede and reference indexes), a canon index memoised per canon build, and the
//! detection context that joins them to the published state.
use super::{DetectScope, SideRef};
use crate::brief::{effective_lifecycle, effective_stable};
use crate::{InflightSet, Materialized, RdyError, ReadState, Scope};
use imbl::OrdMap;
use rdy_graph::{DerivedStatus, Edge, EdgeKind, Graph, IMPACT_LINKS, SUSPECT_LINKS, edges_of};
use rdy_model::{Block, BlockId, FieldValue, Hash, Op, decision};
use rdy_schema::{Pack, Trait};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// One in-flight change's write of one block: its base (`None` for a create), the version it
/// writes and the block (`None` for a delete), and whether it is live: current in some view of
/// the change. A change that writes a block twice is one write: the first op's base, the last
/// op's block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub change: Hash,
    pub block: BlockId,
    pub base: Option<Hash>,
    pub version: Option<Hash>,
    pub new: Option<Arc<Block>>,
    pub live: bool,
}

/// What the detectors read about one in-flight change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeFacts {
    pub id: Hash,
    pub deps: Vec<Hash>,
    pub actor: String,
    pub run: Option<String>,
    pub justified: bool,
    /// The open views whose set holds the change, sorted.
    pub views: Vec<String>,
    /// The blocks the change writes, sorted.
    pub blocks: Vec<BlockId>,
}

/// A live decision write that supersedes a target, with the part it names (`None`: the whole).
pub type Superseding = (Arc<Write>, Option<String>);

/// One change's writes, by block: the first op's base and the last op's block.
type ChangeWrites = BTreeMap<BlockId, (Option<Hash>, Option<Block>)>;

/// The writes of every in-flight change, indexed by block and by change, with the indexes the
/// detectors share (built from live writes only).
#[derive(Clone, Debug, Default)]
pub struct Writes {
    /// Every write on a block, by change.
    by_block: OrdMap<BlockId, Vec<Arc<Write>>>,
    /// Every write of a change, by block.
    by_change: OrdMap<Hash, Vec<Arc<Write>>>,
    changes: OrdMap<Hash, ChangeFacts>,
    /// Live writes whose block defines a name, by normalized name.
    named: OrdMap<String, Vec<Arc<Write>>>,
    /// Live writes whose block is normative, by normalized text.
    texted: OrdMap<Hash, Vec<Arc<Write>>>,
    /// Live decision writes that supersede a target, by target, with the part superseded.
    superseding: OrdMap<BlockId, Vec<Superseding>>,
    /// Live writes whose block has a premise or pointing edge to a block, by that block.
    pointing: OrdMap<BlockId, Vec<(Arc<Write>, Edge)>>,
}

impl Writes {
    /// No writes: what the canon scope reads.
    pub fn empty() -> Writes {
        Writes::default()
    }

    /// The writes of the changes in `set` (each read once), their liveness and the indexes. A
    /// write `(c, b)` is live when some view of `c` includes no other change that writes `b` and
    /// lists `c` in its deps.
    pub fn build(st: &ReadState, set: &InflightSet) -> Result<Writes, RdyError> {
        let mut read = Vec::with_capacity(set.changes.len());
        for c in &set.changes {
            read.push((*c, st.reader().change(c)?));
        }
        // each change's writes, one per block: the first op's base, the last op's block
        let mut per_change: BTreeMap<Hash, ChangeWrites> = BTreeMap::new();
        for (c, change) in &read {
            let blocks = per_change.entry(*c).or_default();
            for op in &change.ops {
                let new = match op {
                    Op::Create { block } | Op::Update { block, .. } => Some(block.clone()),
                    Op::Delete { .. } => None,
                };
                blocks
                    .entry(op.block_id().clone())
                    .and_modify(|w| w.1 = new.clone())
                    .or_insert((op.base(), new));
            }
        }
        // the views in which a later change overwrote (c, b): it writes b and depends on c
        let no_views: Vec<String> = vec![];
        let views_of = |c: &Hash| set.views_of.get(c).unwrap_or(&no_views);
        let mut overwritten: HashMap<(Hash, &BlockId), BTreeSet<&str>> = HashMap::new();
        for (r, change) in &read {
            for b in per_change[r].keys() {
                for d in &change.deps {
                    if d != r && per_change.get(d).is_some_and(|w| w.contains_key(b)) {
                        overwritten
                            .entry((*d, b))
                            .or_default()
                            .extend(views_of(r).iter().map(String::as_str));
                    }
                }
            }
        }
        let pack = st.pack();
        let min_words = pack.duplicate_text_min_words();
        let mut w = Writes::default();
        for (c, change) in &read {
            let views = views_of(c);
            let mut blocks = vec![];
            let mut own = vec![];
            for (b, (base, new)) in &per_change[c] {
                let gone = overwritten.get(&(*c, b));
                let live = views
                    .iter()
                    .any(|v| gone.is_none_or(|g| !g.contains(v.as_str())));
                let write = Arc::new(Write {
                    change: *c,
                    block: b.clone(),
                    base: *base,
                    version: new.as_ref().map(Block::version_hash),
                    new: new.clone().map(Arc::new),
                    live,
                });
                w.by_block
                    .entry(b.clone())
                    .or_default()
                    .push(Arc::clone(&write));
                blocks.push(b.clone());
                if live {
                    w.index_live(pack, min_words, &write);
                }
                own.push(write);
            }
            w.by_change.insert(*c, own);
            w.changes.insert(
                *c,
                ChangeFacts {
                    id: *c,
                    deps: change.deps.clone(),
                    actor: change.provenance.actor.clone(),
                    run: change.provenance.run.clone(),
                    justified: change.justification.is_some(),
                    views: views.clone(),
                    blocks,
                },
            );
        }
        Ok(w)
    }

    /// Adds live write `w` to the name, text, supersede and reference indexes.
    fn index_live(&mut self, pack: &Pack, min_words: usize, w: &Arc<Write>) {
        let Some(b) = w.new.as_deref() else {
            return;
        };
        if defines(pack, b)
            && let Some(name) = b.field_str("name")
        {
            self.named
                .entry(normalize_name(name))
                .or_default()
                .push(Arc::clone(w));
        }
        if is_normative(pack, b)
            && let Some(h) = normalize_text(&b.body, min_words)
        {
            self.texted.entry(h).or_default().push(Arc::clone(w));
        }
        if b.block_type == decision::DECISION {
            for l in b.links.get(decision::SUPERSEDES).into_iter().flatten() {
                self.superseding
                    .entry(l.target.clone())
                    .or_default()
                    .push((Arc::clone(w), l.part.clone()));
            }
        }
        for e in edges_of(b) {
            if premise_edge(&e) || pointing_edge(&e) {
                self.pointing
                    .entry(e.to.clone())
                    .or_default()
                    .push((Arc::clone(w), e));
            }
        }
    }

    /// Every live write, by block, then change.
    pub fn live(&self) -> impl Iterator<Item = &Arc<Write>> {
        self.by_block.values().flatten().filter(|w| w.live)
    }

    /// The live writes on `b`, by change.
    pub fn live_on(&self, b: &BlockId) -> Vec<Arc<Write>> {
        self.all_on(b).into_iter().filter(|w| w.live).collect()
    }

    /// Every write on `b`, live or not, by change.
    pub fn all_on(&self, b: &BlockId) -> Vec<Arc<Write>> {
        self.by_block.get(b).cloned().unwrap_or_default()
    }

    /// Every write of change `c`, live or not, by block.
    pub fn of_change(&self, c: &Hash) -> Vec<Arc<Write>> {
        self.by_change.get(c).cloned().unwrap_or_default()
    }

    /// What the detectors read about in-flight change `c`.
    pub fn change(&self, c: &Hash) -> Option<&ChangeFacts> {
        self.changes.get(c)
    }

    /// Every in-flight change, by id.
    pub fn changes(&self) -> impl Iterator<Item = &ChangeFacts> {
        self.changes.values()
    }

    /// Whether changes `a` and `b` share an open view.
    pub fn common_view(&self, a: &Hash, b: &Hash) -> bool {
        match (self.changes.get(a), self.changes.get(b)) {
            (Some(x), Some(y)) => x.views.iter().any(|v| y.views.contains(v)),
            _ => false,
        }
    }

    /// The live writes whose block defines `normalized` (a [`normalize_name`] result).
    pub fn named(&self, normalized: &str) -> Vec<Arc<Write>> {
        self.named.get(normalized).cloned().unwrap_or_default()
    }

    /// The live writes whose normative block's text normalizes to `h` ([`normalize_text`]).
    pub fn texted(&self, h: &Hash) -> Vec<Arc<Write>> {
        self.texted.get(h).cloned().unwrap_or_default()
    }

    /// The live decision writes that supersede `t`, each with the part it names.
    pub fn superseding(&self, t: &BlockId) -> Vec<Superseding> {
        self.superseding.get(t).cloned().unwrap_or_default()
    }

    /// The live writes whose block has a pointing or premise edge to `x`, with that edge.
    pub fn pointing_at(&self, x: &BlockId) -> Vec<(Arc<Write>, Edge)> {
        self.pointing.get(x).cloned().unwrap_or_default()
    }
}

/// Canon's names, normative texts and supersedes, built once per canon build.
#[derive(Clone, Debug, Default)]
pub struct CanonIndex {
    /// Blocks that define a name, by normalized name.
    pub names: BTreeMap<String, Vec<BlockId>>,
    /// Normative blocks, by normalized text.
    pub texts: BTreeMap<Hash, Vec<BlockId>>,
    /// For each target, the canon decisions that supersede it (with the part) and whose canon
    /// status is neither rejected, superseded nor revoked.
    pub supersedes: BTreeMap<BlockId, Vec<(BlockId, Option<String>)>>,
}

impl CanonIndex {
    /// The index of canon's materialization `m`, whose graph is `g`.
    pub(crate) fn build(pack: &Pack, m: &Materialized, g: &Graph) -> CanonIndex {
        let min_words = pack.duplicate_text_min_words();
        let mut ix = CanonIndex::default();
        for b in m.iter() {
            if defines(pack, b)
                && let Some(name) = b.field_str("name")
            {
                ix.names
                    .entry(normalize_name(name))
                    .or_default()
                    .push(b.id.clone());
            }
            if is_normative(pack, b)
                && let Some(h) = normalize_text(&b.body, min_words)
            {
                ix.texts.entry(h).or_default().push(b.id.clone());
            }
            if b.block_type == decision::DECISION
                && !matches!(
                    g.status(&b.id),
                    Some(
                        DerivedStatus::Rejected
                            | DerivedStatus::Superseded { .. }
                            | DerivedStatus::Revoked { .. }
                    )
                )
            {
                for l in b.links.get(decision::SUPERSEDES).into_iter().flatten() {
                    ix.supersedes
                        .entry(l.target.clone())
                        .or_default()
                        .push((b.id.clone(), l.part.clone()));
                }
            }
        }
        ix
    }
}

/// Canon's index in `st`, memoised in canon's build.
pub(crate) fn canon_index_of(st: &ReadState) -> Result<Arc<CanonIndex>, RdyError> {
    let built = st.built(&Scope::Canon)?;
    if let Some(ix) = built.tension_index.get() {
        return Ok(Arc::clone(ix));
    }
    let g = st.graph(&Scope::Canon)?;
    Ok(Arc::clone(built.tension_index.get_or_init(|| {
        Arc::new(CanonIndex::build(st.pack(), &built.m, &g))
    })))
}

/// Whether `b` is normative content: its type has the `normative` trait, or it is a `table`
/// marked `normative: true`.
fn is_normative(pack: &Pack, b: &Block) -> bool {
    has_trait(pack, b, Trait::Normative)
        || (b.block_type == "table" && b.fields.get("normative") == Some(&FieldValue::Bool(true)))
}

/// Whether `b` defines a name: its type has the `defines` trait and it has a string `name`.
fn defines(pack: &Pack, b: &Block) -> bool {
    has_trait(pack, b, Trait::Defines) && b.field_str("name").is_some()
}

fn has_trait(pack: &Pack, b: &Block, t: Trait) -> bool {
    pack.block_types
        .get(&b.block_type)
        .is_some_and(|d| d.traits.contains(&t))
}

/// What one detection run reads: the published state, the in-flight writes (none in the canon
/// scope) and canon's index.
pub struct DetectCx<'a> {
    pub st: &'a ReadState,
    pub writes: &'a Writes,
    canon: Arc<CanonIndex>,
    scope: DetectScope,
}

impl<'a> DetectCx<'a> {
    pub(crate) fn new(
        st: &'a ReadState,
        writes: &'a Writes,
        canon: Arc<CanonIndex>,
        scope: DetectScope,
    ) -> DetectCx<'a> {
        DetectCx {
            st,
            writes,
            canon,
            scope,
        }
    }

    /// The scope this run detects in.
    pub fn scope(&self) -> DetectScope {
        self.scope
    }

    /// Canon's index.
    pub fn canon_index(&self) -> &CanonIndex {
        &self.canon
    }

    pub fn canon_m(&self) -> Result<Arc<Materialized>, RdyError> {
        self.st.materialized(&Scope::Canon)
    }

    pub fn inflight_m(&self) -> Result<Arc<Materialized>, RdyError> {
        self.st.materialized(&Scope::Inflight)
    }

    pub fn canon_graph(&self) -> Result<Arc<Graph>, RdyError> {
        self.st.graph(&Scope::Canon)
    }

    pub fn inflight_graph(&self) -> Result<Arc<Graph>, RdyError> {
        self.st.graph(&Scope::Inflight)
    }

    /// The scope `stable`, `deprecated` and `side_at` read: the in-flight one, or canon in the
    /// canon scope.
    fn current(&self) -> Scope {
        match self.scope {
            DetectScope::Inflight => Scope::Inflight,
            DetectScope::Canon => Scope::Canon,
        }
    }

    /// Canon's side of `b` and its canon version: the landing that carried that version (of
    /// its producer), when known. `None` when canon has no `b`.
    pub fn canon_side(&self, b: &BlockId) -> Result<Option<(SideRef, Hash)>, RdyError> {
        let Some(v) = self.canon_m()?.version_of(b) else {
            return Ok(None);
        };
        let reader = self.st.reader();
        let landing = match reader.producer(&v)? {
            Some(c) => reader.landing_of(&c)?,
            None => None,
        };
        Ok(Some((SideRef::Canon { landing }, v)))
    }

    /// The side that holds `b`'s current version (in flight, or canon's in the canon scope) and
    /// that version (`None`: an in-flight delete). In flight: the live write of that version
    /// (the merge's last writer first), else any write of it, else canon's side when canon has
    /// that version. `None` when `b` is absent and nothing deletes it.
    pub fn side_at(&self, b: &BlockId) -> Result<Option<(SideRef, Option<Hash>)>, RdyError> {
        if self.scope == DetectScope::Canon {
            return Ok(self.canon_side(b)?.map(|(s, v)| (s, Some(v))));
        }
        let v = self.inflight_m()?.version_of(b);
        let last = self
            .st
            .inflight_cell
            .last
            .get()
            .and_then(|l| l.get(b).copied());
        let live = self.writes.live_on(b);
        let matching: Vec<&Arc<Write>> = live.iter().filter(|w| w.version == v).collect();
        if let Some(w) = matching
            .iter()
            .find(|w| Some(w.change) == last)
            .or(matching.first())
        {
            return Ok(Some((SideRef::Change(w.change), v)));
        }
        if let Some(w) = self.writes.all_on(b).iter().find(|w| w.version == v) {
            return Ok(Some((SideRef::Change(w.change), v)));
        }
        let Some(v) = v else {
            return Ok(None);
        };
        match self.canon_side(b)? {
            Some((side, cv)) if cv == v => Ok(Some((side, Some(v)))),
            _ => Ok(Some((SideRef::Canon { landing: None }, Some(v)))),
        }
    }

    /// Whether `b`'s effective lifecycle is `stable` (in flight, falling back to canon's when
    /// `b` is absent there; canon's in the canon scope).
    pub fn stable(&self, b: &BlockId) -> Result<bool, RdyError> {
        let m = self.st.materialized(&self.current())?;
        if m.get(b).is_some() {
            return Ok(effective_stable(&m, Some(b)));
        }
        self.stable_before(b)
    }

    /// Whether `b`'s effective lifecycle is `stable` in canon.
    pub fn stable_before(&self, b: &BlockId) -> Result<bool, RdyError> {
        Ok(effective_stable(&*self.canon_m()?, Some(b)))
    }

    /// Whether `b` is deprecated: its effective lifecycle is `deprecated`, or it is a decision
    /// wholly superseded or revoked (a partial supersede does not count). Read in flight, or in
    /// canon in the canon scope.
    pub fn deprecated(&self, b: &BlockId) -> Result<bool, RdyError> {
        let scope = self.current();
        let m = self.st.materialized(&scope)?;
        if effective_lifecycle(&m, Some(b)).as_deref() == Some("deprecated") {
            return Ok(true);
        }
        Ok(matches!(
            self.st.graph(&scope)?.status(b),
            Some(DerivedStatus::Superseded { .. } | DerivedStatus::Revoked { .. })
        ))
    }

    /// Whether `b` is normative content (a `normative` type, or a `table` with `normative: true`).
    pub fn is_normative(&self, b: &Block) -> bool {
        is_normative(self.st.pack(), b)
    }

    /// Whether `b` defines a name (a `defines` type with a string `name` field).
    pub fn defines(&self, b: &Block) -> bool {
        defines(self.st.pack(), b)
    }

    /// The pack areas of `b` (in flight, or in canon in the canon scope).
    pub fn areas_of(&self, b: &BlockId) -> Result<Vec<String>, RdyError> {
        self.st.areas_of(&self.current(), b)
    }
}

/// A name as duplicates compare it: lowercase, whitespace runs as one space, trimmed.
pub fn normalize_name(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A normative text as duplicates compare it: the hash of its normalized form ([`normalize_name`]
/// applied to the body), or `None` when that has fewer than `min_words` words (an empty body
/// included).
pub fn normalize_text(s: &str, min_words: usize) -> Option<Hash> {
    let n = normalize_name(s);
    let words = n.split_whitespace().count();
    if words == 0 || words < min_words {
        return None;
    }
    Some(Hash::of(n.as_bytes()))
}

/// A premise edge: a reference, or a link in `SUSPECT_LINKS`.
pub fn premise_edge(e: &Edge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => SUSPECT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

/// A pointing edge: a reference, or a link in `IMPACT_LINKS`.
pub fn pointing_edge(e: &Edge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => IMPACT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}
