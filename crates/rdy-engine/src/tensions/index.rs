//! The shared index the detectors read: the in-flight writes (with their liveness and their name,
//! text, supersede and reference indexes), a canon index memoised per canon build, and the
//! detection context that joins them to the published state.
use super::drift::ShingleIndex;
use super::{DetectScope, Involved, SideRef};
use crate::brief::{effective_lifecycle, effective_stable};
use crate::lexicon::Lexicon;
use crate::{InflightSet, Materialized, RdyError, ReadState, Scope};
use imbl::OrdMap;
use rdy_graph::{DerivedStatus, Edge, EdgeKind, Graph, IMPACT_LINKS, SUSPECT_LINKS, edges_of};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, Op, decision};
use rdy_schema::{Pack, Trait};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

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
    /// Live decision writes that supersede or revoke a target, by target, each once.
    retiring: OrdMap<BlockId, Vec<Arc<Write>>>,
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
        let per_change: BTreeMap<Hash, ChangeWrites> = read
            .iter()
            .map(|(c, change)| (*c, writes_of(change)))
            .collect();
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

    /// Whether `set` is the set these writes were built from plus `change` (new, and in no view
    /// that held these changes): every other change in the same views.
    pub(crate) fn extends_to(&self, set: &InflightSet, change: &Hash) -> bool {
        set.changes.len() == self.changes.len() + 1
            && !self.changes.contains_key(change)
            && set.views_of.contains_key(change)
            && self
                .changes
                .values()
                .all(|f| set.views_of.get(&f.id) == Some(&f.views))
    }

    /// These writes with in-flight change `change` (just committed to `view`) added: its writes,
    /// the liveness of every write on the blocks it writes, and their index entries, as
    /// [`Writes::build`] over the state's in-flight set computes them. When that set is not
    /// these writes' set plus `change`, the full build.
    pub fn with_change(
        &self,
        st: &ReadState,
        change: Hash,
        view: &str,
    ) -> Result<Writes, RdyError> {
        let set = st.inflight_set()?;
        if !self.extends_to(&set, &change) {
            return Writes::build(st, &set);
        }
        let read = st.reader().change(&change)?;
        let mine = writes_of(&read);
        let views = set
            .views_of
            .get(&change)
            .cloned()
            .unwrap_or_else(|| vec![view.to_string()]);
        let pack = st.pack();
        let min_words = pack.duplicate_text_min_words();
        let mut w = self.clone();
        w.changes.insert(
            change,
            ChangeFacts {
                id: change,
                deps: read.deps.clone(),
                actor: read.provenance.actor.clone(),
                run: read.provenance.run.clone(),
                justified: read.justification.is_some(),
                views,
                blocks: mine.keys().cloned().collect(),
            },
        );
        let mut own = Vec::with_capacity(mine.len());
        let mut touched = IndexKeys::default();
        for (b, (base, new)) in mine {
            let before = w.by_block.get(&b).cloned().unwrap_or_default();
            // the old live writes on b leave the indexes
            for x in before.iter().filter(|x| x.live) {
                touched.add(pack, min_words, x);
            }
            w.unindex_block(&touched, &b);
            let at = before.partition_point(|x| x.change < change);
            let mut writes: Vec<Arc<Write>> = before;
            writes.insert(
                at,
                Arc::new(Write {
                    change,
                    block: b.clone(),
                    base,
                    version: new.as_ref().map(Block::version_hash),
                    new: new.map(Arc::new),
                    live: true,
                }),
            );
            // the liveness of every write on b, from the changes that write b
            let writers: Vec<Hash> = writes.iter().map(|x| x.change).collect();
            let mut next = Vec::with_capacity(writes.len());
            for x in &writes {
                let views = w
                    .changes
                    .get(&x.change)
                    .map(|f| f.views.as_slice())
                    .unwrap_or(&[]);
                let gone: BTreeSet<&str> = writers
                    .iter()
                    .filter(|r| **r != x.change)
                    .filter_map(|r| w.changes.get(r))
                    .filter(|f| f.deps.contains(&x.change))
                    .flat_map(|f| f.views.iter().map(String::as_str))
                    .collect();
                let live = views.iter().any(|v| !gone.contains(v.as_str()));
                let y = if live == x.live {
                    Arc::clone(x)
                } else {
                    Arc::new(Write {
                        live,
                        ..(**x).clone()
                    })
                };
                next.push(y);
            }
            for y in &next {
                if y.live {
                    touched.add(pack, min_words, y);
                    w.index_live(pack, min_words, y);
                }
                if y.change == change {
                    own.push(Arc::clone(y));
                } else if let Some(list) = w.by_change.get_mut(&y.change)
                    && let Some(slot) = list.iter_mut().find(|z| z.block == b)
                {
                    *slot = Arc::clone(y);
                }
            }
            w.by_block.insert(b, next);
        }
        w.by_change.insert(change, own);
        w.sort_index(&touched);
        Ok(w)
    }

    /// Removes every entry of a write on `b` from the index entries `keys` names.
    fn unindex_block(&mut self, keys: &IndexKeys, b: &BlockId) {
        for k in &keys.names {
            retain_key(&mut self.named, k, |x| &x.block != b);
        }
        for k in &keys.texts {
            retain_key(&mut self.texted, k, |x| &x.block != b);
        }
        for k in &keys.targets {
            retain_key(&mut self.superseding, k, |(x, _)| &x.block != b);
        }
        for k in &keys.retired {
            retain_key(&mut self.retiring, k, |x| &x.block != b);
        }
        for k in &keys.pointed {
            retain_key(&mut self.pointing, k, |(x, _)| &x.block != b);
        }
    }

    /// Orders the index entries `keys` names as a build does: by change, then block (a stable
    /// sort keeps one write's edges in their order).
    fn sort_index(&mut self, keys: &IndexKeys) {
        fn order(x: &Write, y: &Write) -> std::cmp::Ordering {
            (x.change, &x.block).cmp(&(y.change, &y.block))
        }
        for k in &keys.names {
            if let Some(v) = self.named.get_mut(k) {
                v.sort_by(|x, y| order(x, y));
            }
        }
        for k in &keys.texts {
            if let Some(v) = self.texted.get_mut(k) {
                v.sort_by(|x, y| order(x, y));
            }
        }
        for k in &keys.targets {
            if let Some(v) = self.superseding.get_mut(k) {
                v.sort_by(|(x, _), (y, _)| order(x, y));
            }
        }
        for k in &keys.retired {
            if let Some(v) = self.retiring.get_mut(k) {
                v.sort_by(|x, y| order(x, y));
            }
        }
        for k in &keys.pointed {
            if let Some(v) = self.pointing.get_mut(k) {
                v.sort_by(|(x, _), (y, _)| order(x, y));
            }
        }
    }

    /// Adds live write `w` to the name, text, supersede and reference indexes (`IndexKeys::add`
    /// names the same keys).
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
        if let Some(h) = text_key(pack, b, min_words) {
            self.texted.entry(h).or_default().push(Arc::clone(w));
        }
        if b.block_type == decision::DECISION {
            for l in b.links.get(decision::SUPERSEDES).into_iter().flatten() {
                self.superseding
                    .entry(l.target.clone())
                    .or_default()
                    .push((Arc::clone(w), l.part.clone()));
            }
            for t in retired_by(b) {
                let list = self.retiring.entry(t).or_default();
                if !list.iter().any(|x| Arc::ptr_eq(x, w)) {
                    list.push(Arc::clone(w));
                }
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

    /// The live writes with a block that `m` does not hold at their version: a conflicted block
    /// (two live writes, one kept), a block a later change in another view overwrote, or a write
    /// the materialization does not hold. Their text is in no index built from `m`. Reads no text.
    pub fn unmaterialized(&self, m: &Materialized) -> Vec<Arc<Write>> {
        self.live()
            .filter(|w| w.new.is_some() && m.version_of(&w.block) != w.version)
            .cloned()
            .collect()
    }

    /// Every block some write (live or not) writes, by id.
    pub fn blocks(&self) -> impl Iterator<Item = &BlockId> {
        self.by_block.keys()
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

    /// The blocks of the live writes that define a name.
    pub fn named_ids(&self) -> impl Iterator<Item = &BlockId> {
        self.named.values().flatten().map(|w| &w.block)
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

    /// The live decision writes that supersede or revoke `t`, each once, by change.
    pub fn retiring(&self, t: &BlockId) -> Vec<Arc<Write>> {
        self.retiring.get(t).cloned().unwrap_or_default()
    }

    /// The live writes whose block has a pointing or premise edge to `x`, with that edge.
    pub fn pointing_at(&self, x: &BlockId) -> Vec<(Arc<Write>, Edge)> {
        self.pointing.get(x).cloned().unwrap_or_default()
    }
}

/// One change's writes, one per block: the first op's base, the last op's block.
fn writes_of(change: &Change) -> ChangeWrites {
    let mut blocks = ChangeWrites::new();
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
    blocks
}

/// The blocks decision `b` supersedes or revokes (none for another block type), each once.
pub fn retired_by(b: &Block) -> BTreeSet<BlockId> {
    if b.block_type != decision::DECISION {
        return BTreeSet::new();
    }
    [decision::SUPERSEDES, decision::REVOKES]
        .into_iter()
        .flat_map(|k| b.links.get(k).into_iter().flatten())
        .map(|l| l.target.clone())
        .collect()
}

/// Keeps the entries of `map[k]` that `keep` accepts; a key left with none is removed.
fn retain_key<K: Ord + Clone, T: Clone>(
    map: &mut OrdMap<K, Vec<T>>,
    k: &K,
    keep: impl Fn(&T) -> bool,
) {
    let empty = match map.get_mut(k) {
        Some(v) => {
            v.retain(|x| keep(x));
            v.is_empty()
        }
        None => false,
    };
    if empty {
        map.remove(k);
    }
}

/// The index keys some live writes are filed under.
#[derive(Default)]
struct IndexKeys {
    names: BTreeSet<String>,
    texts: BTreeSet<Hash>,
    targets: BTreeSet<BlockId>,
    retired: BTreeSet<BlockId>,
    pointed: BTreeSet<BlockId>,
}

impl IndexKeys {
    /// Adds the keys `index_live` files live write `w` under (keep the two in step).
    fn add(&mut self, pack: &Pack, min_words: usize, w: &Write) {
        let Some(b) = w.new.as_deref() else {
            return;
        };
        if defines(pack, b)
            && let Some(name) = b.field_str("name")
        {
            self.names.insert(normalize_name(name));
        }
        if let Some(h) = text_key(pack, b, min_words) {
            self.texts.insert(h);
        }
        if b.block_type == decision::DECISION {
            for l in b.links.get(decision::SUPERSEDES).into_iter().flatten() {
                self.targets.insert(l.target.clone());
            }
            self.retired.extend(retired_by(b));
        }
        for e in edges_of(b) {
            if premise_edge(&e) || pointing_edge(&e) {
                self.pointed.insert(e.to.clone());
            }
        }
    }
}

/// Canon's names, normative texts and supersedes, built once per canon build, and canon's
/// shingle index (restatement drift), built on first read.
#[derive(Clone, Debug, Default)]
pub struct CanonIndex {
    /// Blocks that define a name, by normalized name.
    pub names: BTreeMap<String, Vec<BlockId>>,
    /// Normative blocks, by normalized text.
    pub texts: BTreeMap<Hash, Vec<BlockId>>,
    /// For each target, the canon decisions that supersede it (with the part) and whose canon
    /// status is neither rejected, superseded nor revoked.
    pub supersedes: BTreeMap<BlockId, Vec<(BlockId, Option<String>)>>,
    /// The blocks that take part in restatement drift, by shingle; lazy (see
    /// [`DetectCx::canon_shingles`]).
    shingles: OnceLock<Arc<ShingleIndex>>,
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
            if let Some(h) = text_key(pack, b, min_words) {
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
pub(crate) fn is_normative(pack: &Pack, b: &Block) -> bool {
    has_trait(pack, b, Trait::Normative)
        || (b.block_type == "table" && b.fields.get("normative") == Some(&FieldValue::Bool(true)))
}

/// The normative text duplicates compare for `b`: a decision's `statement` (never its body,
/// founder ruling 2026-10-08: register boilerplate in bodies is not a duplicate), any other
/// normative block's body; `None` for a block that is not normative or a decision without a
/// statement.
fn duplicate_text<'b>(pack: &Pack, b: &'b Block) -> Option<&'b str> {
    if b.block_type == decision::DECISION {
        return b.field_str("statement");
    }
    is_normative(pack, b).then_some(b.body.as_str())
}

/// The text key `b` is grouped under: [`normalize_text`] of its [`duplicate_text`].
pub(crate) fn text_key(pack: &Pack, b: &Block, min_words: usize) -> Option<Hash> {
    normalize_text(duplicate_text(pack, b)?, min_words)
}

/// Whether `b` defines a name: its type has the `defines` trait and it has a string `name`.
fn defines(pack: &Pack, b: &Block) -> bool {
    has_trait(pack, b, Trait::Defines) && b.field_str("name").is_some()
}

/// Whether `b`'s type has trait `t` in `pack`.
pub(crate) fn has_trait(pack: &Pack, b: &Block, t: Trait) -> bool {
    pack.block_types
        .get(&b.block_type)
        .is_some_and(|d| d.traits.contains(&t))
}

/// A landing run's canon-to-be: its materialization and graph, and the blocks its ops touch (the
/// lexicon of that canon is canon's with those blocks swapped).
struct Landing {
    m: Arc<Materialized>,
    g: Arc<Graph>,
    touched: BTreeSet<BlockId>,
}

/// The shingle index of the blocks a run's scope moved away from canon (restatement drift): every
/// block a write of the run writes whose version there is not canon's, and the index of those of
/// them that take part. Empty in the canon scope.
#[derive(Debug, Default)]
pub struct MovedShingles {
    pub blocks: BTreeSet<BlockId>,
    pub index: ShingleIndex,
}

/// What one detection run reads: the published state, the in-flight writes (none in the canon
/// scope) and canon's index.
pub struct DetectCx<'a> {
    pub st: &'a ReadState,
    pub writes: &'a Writes,
    canon: Arc<CanonIndex>,
    scope: DetectScope,
    /// A landing's run (tgat1): the canon its landing set would give, read where the in-flight
    /// scope is read otherwise.
    landing: Option<Landing>,
    /// The entity lexicon of the scope this run reads (see [`DetectCx::lexicon`]); lazy.
    lexicon: OnceLock<Arc<Lexicon>>,
    /// The shingle index of the blocks this run's scope moved (see
    /// [`DetectCx::moved_shingles`]); lazy.
    moved: OnceLock<Arc<MovedShingles>>,
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
            landing: None,
            lexicon: OnceLock::new(),
            moved: OnceLock::new(),
        }
    }

    /// An in-flight run over a landing set (tgat1): `writes` are that set's writes, and `m` and
    /// `g` the canon its landing would give, read in place of the in-flight scope; `touched` are
    /// the blocks the landing's ops write.
    pub(crate) fn landing(
        st: &'a ReadState,
        writes: &'a Writes,
        canon: Arc<CanonIndex>,
        m: Arc<Materialized>,
        g: Arc<Graph>,
        touched: BTreeSet<BlockId>,
    ) -> DetectCx<'a> {
        DetectCx {
            landing: Some(Landing { m, g, touched }),
            ..DetectCx::new(st, writes, canon, DetectScope::Inflight)
        }
    }

    /// The entity lexicon of the materialization this run reads: canon's in the canon scope, the
    /// in-flight one in the in-flight scope (a landing's run: that of the canon the landing would
    /// give). Memoised in this run, and read through the state's own lexicon where it has one.
    pub fn lexicon(&self) -> Result<Arc<Lexicon>, RdyError> {
        if let Some(l) = self.lexicon.get() {
            return Ok(Arc::clone(l));
        }
        let l = match (&self.landing, self.scope) {
            (_, DetectScope::Canon) => self.st.lexicon(&Scope::Canon)?,
            // carried like the landing graph: canon's lexicon with the touched blocks swapped
            (Some(l), DetectScope::Inflight) => {
                let canon_m = self.canon_m()?;
                let canon_l = self.st.lexicon(&Scope::Canon)?;
                Arc::new(canon_l.with_changes(
                    l.touched.iter().filter_map(|b| canon_m.get(b)),
                    l.touched.iter().filter_map(|b| l.m.get(b)),
                ))
            }
            (None, DetectScope::Inflight) => self.st.lexicon(&Scope::Inflight)?,
        };
        Ok(Arc::clone(self.lexicon.get_or_init(|| l)))
    }

    /// The definitions that are deprecated in the scope this run reads, by id. Found through the
    /// names index (canon's and the live writes'), so no text is read and the cost follows the
    /// number of definitions, not of blocks.
    pub fn deprecated_definitions(&self) -> Result<Vec<Block>, RdyError> {
        let m = self.current_m()?;
        let ids: BTreeSet<&BlockId> = self
            .canon
            .names
            .values()
            .flatten()
            .chain(self.writes.named_ids())
            .collect();
        let mut out = vec![];
        for id in ids {
            if let Some(b) = m.get(id)
                && self.defines(b)
                && self.deprecated(id)?
            {
                out.push(b.clone());
            }
        }
        Ok(out)
    }

    /// Canon's shingle index (restatement drift): every canon block that takes part, by
    /// shingle. Built on first read and kept in canon's index, so once per canon build.
    pub fn canon_shingles(&self) -> Result<Arc<ShingleIndex>, RdyError> {
        if let Some(ix) = self.canon.shingles.get() {
            return Ok(Arc::clone(ix));
        }
        let m = self.canon_m()?;
        Ok(Arc::clone(self.canon.shingles.get_or_init(|| {
            Arc::new(ShingleIndex::of(self.st.pack(), m.iter()))
        })))
    }

    /// The shingle index of the blocks this run's scope moved away from canon: with
    /// [`DetectCx::canon_shingles`], the inverted index over the scope's materialization. Every
    /// block whose version there differs from canon's has a write in this run, so the blocks the
    /// writes name are enough. Empty in the canon scope; built on first read in this run.
    pub fn moved_shingles(&self) -> Result<Arc<MovedShingles>, RdyError> {
        if let Some(ix) = self.moved.get() {
            return Ok(Arc::clone(ix));
        }
        let mut moved = MovedShingles::default();
        if self.scope == DetectScope::Inflight {
            let m = self.inflight_m()?;
            let canon = self.canon_m()?;
            moved.blocks = self
                .writes
                .blocks()
                .filter(|b| m.version_of(b) != canon.version_of(b))
                .cloned()
                .collect();
            moved.index =
                ShingleIndex::of(self.st.pack(), moved.blocks.iter().filter_map(|b| m.get(b)));
        }
        Ok(Arc::clone(self.moved.get_or_init(|| Arc::new(moved))))
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

    /// The in-flight materialization (a landing's run: the canon its landing would give). In the
    /// canon scope, canon's: the canon part is memoised per canon build, so a canon-scope run
    /// never reads in-flight data.
    pub fn inflight_m(&self) -> Result<Arc<Materialized>, RdyError> {
        match (&self.landing, self.scope) {
            (_, DetectScope::Canon) => self.canon_m(),
            (Some(l), DetectScope::Inflight) => Ok(Arc::clone(&l.m)),
            (None, DetectScope::Inflight) => self.st.materialized(&Scope::Inflight),
        }
    }

    pub fn canon_graph(&self) -> Result<Arc<Graph>, RdyError> {
        self.st.graph(&Scope::Canon)
    }

    /// The in-flight graph (a landing's run: the graph of the canon its landing would give). In
    /// the canon scope, canon's (see [`DetectCx::inflight_m`]).
    pub fn inflight_graph(&self) -> Result<Arc<Graph>, RdyError> {
        match (&self.landing, self.scope) {
            (_, DetectScope::Canon) => self.canon_graph(),
            (Some(l), DetectScope::Inflight) => Ok(Arc::clone(&l.g)),
            (None, DetectScope::Inflight) => self.st.graph(&Scope::Inflight),
        }
    }

    /// The materialization `stable`, `deprecated` and `areas_of` read: the in-flight one, or
    /// canon's in the canon scope.
    fn current_m(&self) -> Result<Arc<Materialized>, RdyError> {
        self.inflight_m()
    }

    /// The graph of [`DetectCx::current_m`].
    fn current_graph(&self) -> Result<Arc<Graph>, RdyError> {
        self.inflight_graph()
    }

    /// The content hash of `b` as a vkey hashes it: [`Block::content_hash_with`] the tracking
    /// fields, so derived fields (a decision's `status`, `decided_at`), pins and tracking fields
    /// are left out.
    pub fn content_of(&self, b: &Block) -> Hash {
        b.content_hash_with(self.st.reader().tracking())
    }

    /// The involved item for write `w`: its change's side, the version it writes and that
    /// block's content.
    pub fn of_write(&self, w: &Write) -> Involved {
        Involved {
            side: SideRef::Change(w.change),
            block: w.block.clone(),
            version: w.version,
            content: w.new.as_deref().map(|b| self.content_of(b)),
        }
    }

    /// The involved item for `block` at `version` on `side`, with that version's content: from
    /// the side's write when the side is a change, else from the materialized scope that holds
    /// that version, else read from the store. An intent side's content is its version; an
    /// absent version has none.
    pub fn involved(
        &self,
        side: SideRef,
        block: &BlockId,
        version: Option<Hash>,
    ) -> Result<Involved, RdyError> {
        let content = match (&side, version) {
            (_, None) => None,
            (SideRef::Intent { .. }, v) => v,
            (_, Some(v)) => Some(self.content_at(&side, block, &v)?),
        };
        Ok(Involved {
            side,
            block: block.clone(),
            version,
            content,
        })
    }

    /// The content of `block`'s version `v` (see [`DetectCx::involved`]).
    fn content_at(&self, side: &SideRef, block: &BlockId, v: &Hash) -> Result<Hash, RdyError> {
        if let SideRef::Change(c) = side
            && let Some(b) = self
                .writes
                .of_change(c)
                .iter()
                .find(|w| &w.block == block && w.version.as_ref() == Some(v))
                .and_then(|w| w.new.clone())
        {
            return Ok(self.content_of(&b));
        }
        let mut scopes = vec![self.canon_m()?];
        if self.scope == DetectScope::Inflight {
            scopes.push(self.inflight_m()?);
        }
        for m in scopes {
            if m.version_of(block).as_ref() == Some(v)
                && let Some(b) = m.get(block)
            {
                return Ok(self.content_of(b));
            }
        }
        Ok(self.content_of(&self.st.reader().get_block(v)?))
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
    /// that version (`None`: an in-flight delete). In flight: the live write of that version with
    /// the smallest change id, else any write of it (the smallest change id), else canon's side
    /// when canon has that version. A function of the in-flight writes, so two states with one
    /// change set name one side. `None` when `b` is absent and nothing deletes it.
    pub fn side_at(&self, b: &BlockId) -> Result<Option<(SideRef, Option<Hash>)>, RdyError> {
        if self.scope == DetectScope::Canon {
            return Ok(self.canon_side(b)?.map(|(s, v)| (s, Some(v))));
        }
        let v = self.inflight_m()?.version_of(b);
        // writes on a block are in change-id order
        if let Some(w) = self.writes.live_on(b).iter().find(|w| w.version == v) {
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
        let m = self.current_m()?;
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
        let m = self.current_m()?;
        if effective_lifecycle(&m, Some(b)).as_deref() == Some("deprecated") {
            return Ok(true);
        }
        Ok(matches!(
            self.current_graph()?.status(b),
            Some(DerivedStatus::Superseded { .. } | DerivedStatus::Revoked { .. })
        ))
    }

    /// Whether `b` is normative content (a `normative` type, or a `table` with `normative: true`).
    pub fn is_normative(&self, b: &Block) -> bool {
        is_normative(self.st.pack(), b)
    }

    /// The text key duplicates group `b` under (a decision's statement, any other normative
    /// block's body), or `None` when it has none or it is under the pack's word minimum.
    pub fn text_key(&self, b: &Block) -> Option<Hash> {
        text_key(self.st.pack(), b, self.st.pack().duplicate_text_min_words())
    }

    /// The text duplicates compare for `b` (a decision's statement, any other normative block's
    /// body), before normalization.
    pub fn duplicate_text<'b>(&self, b: &'b Block) -> Option<&'b str> {
        duplicate_text(self.st.pack(), b)
    }

    /// Whether `b` defines a name (a `defines` type with a string `name` field).
    pub fn defines(&self, b: &Block) -> bool {
        defines(self.st.pack(), b)
    }

    /// The pack areas of `b` (in flight, or in canon in the canon scope).
    pub fn areas_of(&self, b: &BlockId) -> Result<Vec<String>, RdyError> {
        if self.landing.is_none() {
            let scope = match self.scope {
                DetectScope::Inflight => Scope::Inflight,
                DetectScope::Canon => Scope::Canon,
            };
            return self.st.areas_of(&scope, b);
        }
        let m = self.current_m()?;
        match m.get(b) {
            Some(block) => Ok(self.st.areas_in(&m, block)),
            // the landing deletes b: canon's areas, or E_UNKNOWN_BLOCK
            None => self.st.areas_of(&Scope::Canon, b),
        }
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
