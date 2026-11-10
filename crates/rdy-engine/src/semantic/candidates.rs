//! Candidate pairs (spec §9.3 step 1, D-KJ71P): for each block an in-flight change writes, the blocks most likely to disagree with it, by five fixed rules, at most 20 per change. Deterministic; settled pairs are left to the queue.
use crate::lexicon::{Lexicon, is_content};
use crate::tensions::index::{
    DetectCx, Write, Writes, canon_index_of, is_deprecated, is_normative, premise_edge,
};
use crate::tensions::{DetectScope, SideRef};
use crate::{E_LIMIT, Materialized, RdyError, ReadState, Scope};
use rdy_graph::foreign::foreign_edges_of;
use rdy_graph::{EdgeKind, Graph, SUSPECT_LINKS, edges_of};
use rdy_model::foreign::ForeignId;
use rdy_model::semantic::ForeignPairItem;
use rdy_model::{
    Block, BlockId, FieldValue, Hash, PairItem, PairSide, decision, pair_key, pair_key_sides,
};
use rdy_schema::Pack;
use serde::Serialize;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

/// The most requests one change makes.
pub const MAX_CANDIDATES: usize = 20;
/// The most live in-flight writes one cross-view context indexes (nb5, Review I2).
const MAX_CROSS_VIEW_WRITES: usize = 100_000;
/// The most candidate incidences one change's cross-view phase examines.
const MAX_CROSS_VIEW_VISITS: usize = 1_000_000;
/// The fix of both cross-view bounds.
const CROSS_VIEW_FIX: &str =
    "close the views you no longer need: rdy view close, or rdy_view with op close";

thread_local! {
    static REQUEST_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CROSS_VIEW_INDEX_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: this thread's count of [`ReadState::requests_from`] calls. A test reads the
/// difference over a call, on the thread that ran it (the daemon writer's, through a write).
#[doc(hidden)]
pub fn request_builds() -> u64 {
    REQUEST_BUILDS.with(std::cell::Cell::get)
}

/// Test hook: this thread's count of live writes a cross-view index has visited while it was
/// built. A test reads the difference over a call, on the thread that ran it.
#[doc(hidden)]
pub fn cross_view_index_visits() -> u64 {
    CROSS_VIEW_INDEX_VISITS.with(std::cell::Cell::get)
}

/// Why two blocks are a pair. Declaration order is rank: the first rule that holds names the
/// pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// An edge joins the blocks (a reference, or a link that makes a premise).
    Premise,
    /// Both blocks describe one target.
    Describes,
    /// Both texts name one entity.
    Entity,
    /// Both blocks have one parent.
    Section,
    /// Both blocks are in one area.
    Area,
}

impl Rule {
    /// The rule's wire name.
    pub fn name(&self) -> &'static str {
        match self {
            Rule::Premise => "premise",
            Rule::Describes => "describes",
            Rule::Entity => "entity",
            Rule::Section => "section",
            Rule::Area => "area",
        }
    }
}

/// One block of a pair: the side that holds it now, its version and its content.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item {
    pub block: BlockId,
    pub side: SideRef,
    pub version: Hash,
    pub content: Hash,
}

impl Item {
    /// The side this item is in a stored pair: a local block at its content, or a cited block
    /// (a [`SideRef::Foreign`] item) at the delivered content identity, never its version hash.
    pub fn pair_side(&self) -> PairSide {
        match &self.side {
            SideRef::Foreign { repo, .. } => PairSide::Foreign(ForeignPairItem {
                target: ForeignId::new(*repo, self.block.clone()),
                content: self.content,
            }),
            _ => PairSide::Local(PairItem {
                block: self.block.clone(),
                content: self.content,
            }),
        }
    }
}

/// The key of the pair `items` form: [`pair_key`] when every item is local, so every released
/// local key stays, else [`pair_key_sides`] with the cited repo in it.
pub fn items_key(items: &[Item]) -> Hash {
    pair_key_sides(&items.iter().map(Item::pair_side).collect::<Vec<_>>())
}

/// One pair a change asks a reviewer to judge. `items` are sorted by block; a pair with a cited
/// block lists the local item first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub key: Hash,
    pub items: Vec<Item>,
    pub rule: Rule,
    pub change: Hash,
}

/// The label of a request: `rq-` and the first 12 hex digits of its key.
pub fn request_label(key: &Hash) -> String {
    format!("rq-{}", key.short())
}

/// The lowercase hex of a request label `text` (`rq-` and 6 to 64 hex digits, or the hex alone),
/// or `None` when `text` is no request label.
pub fn request_hex(text: &str) -> Option<String> {
    let hex = text
        .strip_prefix("rq-")
        .unwrap_or(text)
        .to_ascii_lowercase();
    ((6..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// Whether `b` can be one side of a pair: it is normative or content (F4).
pub fn takes_part(pack: &Pack, b: &Block) -> bool {
    is_normative(pack, b) || is_content(pack, b)
}

impl ReadState {
    /// The items of the pair `pair` (two blocks, each at a content) now, sorted by block, as a
    /// request carries them; `None` when no view holds the pair (final ruling 2). A form is
    /// anchored on a live in-flight write of one block at its content: the other block is a live
    /// write at its content in a view the anchor's change shares, else canon's when canon holds
    /// that content and a view of the anchor's change does not touch that block. The first form
    /// by (change id, block) wins, so a request and a submit name the same items. With no such
    /// form, the cross-view form (nb5): a live write of each block at its content, by two changes
    /// that share no view, both sides eligible in their own views and joined by the `premise`,
    /// `describes` or `entity` rule read on those two blocks; the first by the two change ids,
    /// in block order. With neither, canon's items when canon holds both contents and no
    /// in-flight write touches either block (an all-canon pair, which is never a request).
    pub fn pair_now(&self, pair: &[(BlockId, Hash)]) -> Result<Option<Vec<Item>>, RdyError> {
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        pair_now_in(&cx, &SideScopes::default(), pair)
    }

    /// [`Self::pair_now`] for a pair of any sides (repo links). A pair of local sides is
    /// `pair_now`'s. A pair of a local block and a cited block is anchored on a live in-flight
    /// write of the local block at its content (the first by change id) and the cited block's
    /// current delivered version when it holds the judged content; the items list the local one
    /// first. `None` when no write holds the content, the cited block moved, was redacted or
    /// left every live source, or a pair holds two cited sides (a cited pair has no change to
    /// review it in).
    pub fn pair_now_sides(&self, pair: &[PairSide]) -> Result<Option<Vec<Item>>, RdyError> {
        let [x, y] = pair else {
            return Ok(None);
        };
        let (local, cited) = match (x, y) {
            (PairSide::Local(a), PairSide::Local(b)) => {
                return self
                    .pair_now(&[(a.block.clone(), a.content), (b.block.clone(), b.content)]);
            }
            (PairSide::Local(l), PairSide::Foreign(f))
            | (PairSide::Foreign(f), PairSide::Local(l)) => (l, f),
            (PairSide::Foreign(_), PairSide::Foreign(_)) => return Ok(None),
        };
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        let mut on = writes_at(&cx, &local.block, local.content);
        on.sort_by_key(|w| w.change);
        let Some(mine) = on.iter().find_map(|w| write_item(w, local.content)) else {
            return Ok(None);
        };
        let Some((theirs, _)) = cited_item(self, &cited.target)? else {
            return Ok(None);
        };
        if theirs.content != cited.content {
            return Ok(None);
        }
        Ok(Some(vec![mine, theirs]))
    }

    /// The canon items of `pair` when canon holds both blocks at those contents, whatever an open
    /// view writes to them. A check's case reads one state, so another view's edit of a block
    /// does not take a canon pair away; [`Self::pair_now`] keeps its rule for derived requests.
    pub fn pair_in_canon(&self, pair: &[(BlockId, Hash)]) -> Result<Option<Vec<Item>>, RdyError> {
        let [(a, ca), (b, cb)] = pair else {
            return Ok(None);
        };
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        canon_pair(&cx, (a, *ca), (b, *cb))
    }

    /// The best rule that holds now between `a` and `b` in the in-flight scope, or `None` when
    /// none holds or a block is absent, deprecated or does not take part.
    pub fn pair_rule(&self, a: &BlockId, b: &BlockId) -> Result<Option<Rule>, RdyError> {
        let set = self.tensions()?;
        let ctx = Ctx::new(self, set.writes())?;
        let (Some(ba), Some(bb)) = (ctx.m.get(a), ctx.m.get(b)) else {
            return Ok(None);
        };
        if a == b || !ctx.eligible(ba)? || !ctx.eligible(bb)? {
            return Ok(None);
        }
        Ok(Rule::ALL.into_iter().find(|r| ctx.holds(*r, ba, bb)))
    }

    /// The best rule that holds between the two blocks `items` name (two, sorted by block), read
    /// on those exact blocks: a change side's write at its version, in the view that write is
    /// current in; a canon side's block at its version, in the other side's view (in canon when
    /// both are canon's). `None` when an item does not resolve, a block is deprecated or does
    /// not take part, or no rule holds. A case names this rule, not the in-flight scope's.
    pub(crate) fn pair_rule_for_items(&self, items: &[Item]) -> Result<Option<Rule>, RdyError> {
        let [ia, ib] = items else {
            return Ok(None);
        };
        if ia.side.is_foreign() || ib.side.is_foreign() {
            return self.foreign_pair_rule(ia, ib);
        }
        if ia.block == ib.block {
            return Ok(None);
        }
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        let (Some((a, ka)), Some((b, kb))) = (item_block(&cx, ia)?, item_block(&cx, ib)?) else {
            return Ok(None);
        };
        let (ka, kb) = match (ka, kb) {
            (Some(x), Some(y)) => (x, y),
            (Some(x), None) => (x.clone(), x),
            (None, Some(y)) => (y.clone(), y),
            (None, None) => (ReadKey::Canon, ReadKey::Canon),
        };
        let sides = SideScopes::default();
        let (sa, sb) = (sides.get(self, ka)?, sides.get(self, kb)?);
        if !side_eligible(self, &sa, &a)? || !side_eligible(self, &sb, &b)? {
            return Ok(None);
        }
        for rule in Rule::ALL {
            if exact_holds(self, rule, (&a, &sa), (&b, &sb))? {
                return Ok(Some(rule));
            }
        }
        Ok(None)
    }

    /// The rule that holds between a local item and a cited item: `Premise` when the local
    /// block, at its item's version in the change that wrote it, points at the cited block
    /// (a reference or a suspect link other than `reflects`); `None` otherwise. Two cited
    /// items have no rule.
    fn foreign_pair_rule(&self, ia: &Item, ib: &Item) -> Result<Option<Rule>, RdyError> {
        let (local, cited) = match (ia.side.is_foreign(), ib.side.is_foreign()) {
            (false, true) => (ia, ib),
            (true, false) => (ib, ia),
            _ => return Ok(None),
        };
        let SideRef::Foreign { repo, .. } = &cited.side else {
            return Ok(None);
        };
        let target = ForeignId::new(*repo, cited.block.clone());
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        let Some((block, _)) = item_block(&cx, local)? else {
            return Ok(None);
        };
        Ok(foreign_edges_of(&block)
            .iter()
            .any(|e| e.to == target && foreign_premise(&e.kind))
            .then_some(Rule::Premise))
    }

    /// The requests change `change` makes, in order: the pairs of each block it writes, by rule
    /// rank, then the subject's id, then closeness; each pair once; at most [`MAX_CANDIDATES`].
    /// Then, while there is room, the pairs across open views (nb5). A change that is not in
    /// flight makes none. Settled pairs are not removed here.
    pub fn requests_from(&self, change: &Hash) -> Result<Vec<Request>, RdyError> {
        self.requests_from_with(change, &mut CrossViewContext::default())
    }

    /// [`Self::requests_from`] with the cross-view context `cross`, which a queue walk keeps for
    /// all its changes: its indexes are built once, when a change first needs them.
    ///
    /// Phase 1 is the same-view and canon phase, whole. Phase 2, while it leaves room, pairs
    /// each subject with live writes of changes that share no view with `change`, by the
    /// `premise`, `describes` and `entity` rules only, in rule order, then the subject's id, then
    /// the other block's doc (the subject's first, then by path), id and change. A pair key
    /// (blocks and contents) counts once over both phases.
    pub(super) fn requests_from_with(
        &self,
        change: &Hash,
        cross: &mut CrossViewContext,
    ) -> Result<Vec<Request>, RdyError> {
        REQUEST_BUILDS.with(|n| n.set(n.get() + 1));
        let set = self.tensions()?;
        let writes: &Writes = set.writes();
        if writes.change(change).is_none() {
            return Ok(vec![]);
        }
        let CrossViewContext {
            limits,
            sides,
            index,
            ..
        } = cross;
        let ctx = Ctx::new(self, writes)?;
        let mut subjects = vec![];
        for w in writes.of_change(change).iter().filter(|w| w.live) {
            let Some(block) = w.new.clone() else {
                continue;
            };
            if !ctx.eligible(&block)? {
                continue;
            }
            let content = ctx.cx.content_of(&block);
            subjects.push(Subject { block, content });
        }
        subjects.sort_by(|a, b| a.block.id.cmp(&b.block.id));
        let mut b = Builder {
            ctx: &ctx,
            sides,
            change: *change,
            out: vec![],
            seen: BTreeSet::new(),
        };
        'rules: for rule in Rule::ALL {
            for s in &subjects {
                if b.full() {
                    break 'rules;
                }
                match rule {
                    Rule::Premise => b.premise(s)?,
                    Rule::Describes => b.describes(s)?,
                    Rule::Entity => b.entity(s)?,
                    Rule::Section => b.section(s)?,
                    Rule::Area => b.area(s)?,
                }
            }
        }
        let visits = if !b.full() && !subjects.is_empty() && has_cross_change(writes, change) {
            if index.is_none() {
                *index = Some(CrossIndex::build(self, writes, sides, *limits)?);
            }
            let idx = index.as_ref().expect("the index was just built");
            b.cross(idx, *limits, &subjects)?
        } else {
            0
        };
        b.across(&subjects)?;
        let out = b.out;
        #[cfg(test)]
        {
            cross.last_visits = visits;
        }
        #[cfg(not(test))]
        let _ = visits;
        Ok(out)
    }
}

/// The bounds of one cross-view context: the live writes it indexes, once, and the candidate
/// incidences each change's phase 2 examines. [`CrossLimits::FIXED`] outside tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CrossLimits {
    writes: usize,
    visits: usize,
}

impl Default for CrossLimits {
    fn default() -> Self {
        CrossLimits::FIXED
    }
}

impl CrossLimits {
    const FIXED: CrossLimits = CrossLimits {
        writes: MAX_CROSS_VIEW_WRITES,
        visits: MAX_CROSS_VIEW_VISITS,
    };

    fn writes_error(&self) -> RdyError {
        RdyError::new(
            E_LIMIT,
            format!("cross-view review exceeds {} in-flight writes", self.writes),
            CROSS_VIEW_FIX,
        )
    }

    fn visits_error(&self) -> RdyError {
        RdyError::new(
            E_LIMIT,
            format!("cross-view review exceeds {} candidate visits", self.visits),
            CROSS_VIEW_FIX,
        )
    }
}

/// What a queue walk keeps across its changes for the cross-view phase (Review I2): the scopes
/// it has read sides in (each view's lexicon read once) and the cross-view indexes, built on
/// first need. The default holds nothing built.
#[derive(Default)]
pub(super) struct CrossViewContext {
    limits: CrossLimits,
    sides: SideScopes,
    index: Option<CrossIndex>,
    /// The candidate incidences the last change's phase 2 examined.
    #[cfg(test)]
    last_visits: usize,
}

/// The key of a read scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ReadKey {
    Canon,
    Inflight,
    View(String),
}

impl ReadKey {
    fn scope(&self) -> Scope {
        match self {
            ReadKey::Canon => Scope::Canon,
            ReadKey::Inflight => Scope::Inflight,
            ReadKey::View(v) => Scope::View(v.clone()),
        }
    }
}

/// One scope a side is read in: its materialization, and its graph and lexicon on first use.
struct SideScope {
    scope: Scope,
    m: Arc<Materialized>,
    g: OnceCell<Arc<Graph>>,
    lex: OnceCell<Arc<Lexicon>>,
}

impl SideScope {
    fn graph(&self, st: &ReadState) -> Result<Arc<Graph>, RdyError> {
        if let Some(g) = self.g.get() {
            return Ok(Arc::clone(g));
        }
        let g = st.graph(&self.scope)?;
        Ok(Arc::clone(self.g.get_or_init(|| g)))
    }

    fn lexicon(&self, st: &ReadState) -> Result<Arc<Lexicon>, RdyError> {
        if let Some(l) = self.lex.get() {
            return Ok(Arc::clone(l));
        }
        let l = st.lexicon(&self.scope)?;
        Ok(Arc::clone(self.lex.get_or_init(|| l)))
    }

    /// The entities `b`'s text names, read with this scope's lexicon.
    fn entities(&self, st: &ReadState, b: &Block) -> Result<BTreeSet<String>, RdyError> {
        Ok(self
            .lexicon(st)?
            .mentions(b)
            .into_iter()
            .map(|m| m.entity)
            .collect())
    }
}

/// The scopes read so far, by key.
#[derive(Default)]
struct SideScopes {
    by: RefCell<HashMap<ReadKey, Rc<SideScope>>>,
}

impl SideScopes {
    fn get(&self, st: &ReadState, key: ReadKey) -> Result<Rc<SideScope>, RdyError> {
        if let Some(s) = self.by.borrow().get(&key) {
            return Ok(Rc::clone(s));
        }
        let scope = key.scope();
        let s = Rc::new(SideScope {
            m: st.materialized(&scope)?,
            scope,
            g: OnceCell::new(),
            lex: OnceCell::new(),
        });
        self.by.borrow_mut().insert(key, Rc::clone(&s));
        Ok(s)
    }

    /// The scope of write `w`: the first view of its change (sorted) it is current in, or
    /// `None` when it is current in none.
    fn of_write(
        &self,
        st: &ReadState,
        writes: &Writes,
        w: &Write,
    ) -> Result<Option<Rc<SideScope>>, RdyError> {
        match view_of(writes, w) {
            Some(v) => Ok(Some(self.get(st, ReadKey::View(v.to_string()))?)),
            None => Ok(None),
        }
    }

    /// The views whose lexicon this cache has read, sorted.
    #[cfg(test)]
    fn lexicon_views(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .by
            .borrow()
            .iter()
            .filter(|(_, s)| s.lex.get().is_some())
            .filter_map(|(k, _)| match k {
                ReadKey::View(v) => Some(v.clone()),
                _ => None,
            })
            .collect();
        v.sort();
        v
    }
}

/// The first view of `w`'s change (the change's views are sorted) that `w` is current in.
fn view_of<'w>(writes: &'w Writes, w: &Write) -> Option<&'w str> {
    writes
        .change(&w.change)?
        .views
        .iter()
        .find(|v| writes.current_in(w, v))
        .map(String::as_str)
}

/// Whether `b`, read in `at`, can be one side of a pair: it takes part, and neither its own
/// lifecycle nor its state in `at` makes it deprecated.
fn side_eligible(st: &ReadState, at: &SideScope, b: &Block) -> Result<bool, RdyError> {
    if !takes_part(st.pack(), b)
        || b.fields.get("lifecycle") == Some(&FieldValue::Str("deprecated".into()))
    {
        return Ok(false);
    }
    Ok(!is_deprecated(&at.m, &*at.graph(st)?, &b.id))
}

/// Whether `from` has a premise edge to `to`.
fn premise_joins(from: &Block, to: &BlockId) -> bool {
    edges_of(from)
        .iter()
        .any(|e| e.to == *to && premise_edge(e))
}

/// Whether `rule` holds between the exact blocks `a` and `b`, each read in its scope.
fn exact_holds(
    st: &ReadState,
    rule: Rule,
    (a, at): (&Block, &SideScope),
    (b, bt): (&Block, &SideScope),
) -> Result<bool, RdyError> {
    Ok(match rule {
        Rule::Premise => premise_joins(a, &b.id) || premise_joins(b, &a.id),
        Rule::Describes => {
            let ta = described(a);
            described(b).iter().any(|t| ta.contains(t))
        }
        Rule::Entity => {
            let ea = at.entities(st, a)?;
            bt.entities(st, b)?.iter().any(|e| ea.contains(e))
        }
        Rule::Section => a.parent.is_some() && a.parent == b.parent,
        Rule::Area => {
            let aa = st.areas_in(&at.m, a);
            st.areas_in(&bt.m, b).iter().any(|x| aa.contains(x))
        }
    })
}

/// A resolved item: its block, and the key of the scope its side is read in (`None`: the other
/// side's).
type ItemBlock = (Arc<Block>, Option<ReadKey>);

/// The block `item` names and the key of the scope its side is read in: a change side's write
/// at the item's version (its view; the in-flight scope when the write is current in none), or
/// canon's block at it (`None`: read in the other side's scope). `None` when it does not
/// resolve.
fn item_block(cx: &DetectCx<'_>, item: &Item) -> Result<Option<ItemBlock>, RdyError> {
    Ok(match &item.side {
        SideRef::Change(c) => cx
            .writes
            .on(&item.block)
            .iter()
            .find(|w| w.change == *c && w.version == Some(item.version))
            .and_then(|w| {
                let key = match view_of(cx.writes, w) {
                    Some(v) => ReadKey::View(v.to_string()),
                    None => ReadKey::Inflight,
                };
                w.new.clone().map(|b| (b, Some(key)))
            }),
        SideRef::Canon { .. } => {
            let canon = cx.canon_m()?;
            (canon.version_of(&item.block) == Some(item.version))
                .then(|| canon.get(&item.block).cloned())
                .flatten()
                .map(|b| (Arc::new(b), None))
        }
        // a cited block's semantic pair is not read here
        SideRef::Intent { .. } | SideRef::Foreign { .. } => None,
    })
}

/// The cross-view indexes of one context: every live write's facts, and the live writes by the
/// blocks their premise edges point at, by `describes` target and by named entity.
#[derive(Default)]
struct CrossIndex {
    facts: HashMap<(Hash, BlockId), WriteFacts>,
    premise_in: HashMap<BlockId, Vec<Arc<Write>>>,
    describes: HashMap<BlockId, Vec<Arc<Write>>>,
    entities: HashMap<String, Vec<Arc<Write>>>,
}

/// A live write's scope (its view) and the entities its block names there.
struct WriteFacts {
    at: Rc<SideScope>,
    entities: BTreeSet<String>,
}

impl Rule {
    const ALL: [Rule; 5] = [
        Rule::Premise,
        Rule::Describes,
        Rule::Entity,
        Rule::Section,
        Rule::Area,
    ];
    /// The rules that pair across views (QE3).
    const TIES: [Rule; 3] = [Rule::Premise, Rule::Describes, Rule::Entity];
}

/// The item of `w`, a write that holds a block, at content `content`.
fn write_item(w: &Write, content: Hash) -> Option<Item> {
    Some(Item {
        block: w.block.clone(),
        side: SideRef::Change(w.change),
        version: w.version?,
        content,
    })
}

/// Canon's item of `block` when canon holds it at `content`.
fn canon_item(cx: &DetectCx<'_>, block: &BlockId, content: Hash) -> Result<Option<Item>, RdyError> {
    if let Some(b) = cx.canon_m()?.get(block)
        && cx.content_of(b) == content
        && let Some((side, version)) = cx.canon_side(block)?
    {
        return Ok(Some(Item {
            block: block.clone(),
            side,
            version,
            content,
        }));
    }
    Ok(None)
}

/// The live in-flight writes of `block` at `content`, by change id.
fn writes_at(cx: &DetectCx<'_>, block: &BlockId, content: Hash) -> Vec<Arc<Write>> {
    cx.writes
        .live_on(block)
        .into_iter()
        .filter(|w| {
            w.new
                .as_deref()
                .is_some_and(|b| cx.content_of(b) == content)
        })
        .collect()
}

/// A write a form may be anchored on, with the other block's writes at its content, that block
/// and that content.
type Anchor<'w> = (&'w Arc<Write>, &'w Vec<Arc<Write>>, &'w BlockId, Hash);

/// [`ReadState::pair_now`] in the scope `cx` reads, with sides read through `sides`.
fn pair_now_in(
    cx: &DetectCx<'_>,
    sides: &SideScopes,
    pair: &[(BlockId, Hash)],
) -> Result<Option<Vec<Item>>, RdyError> {
    let [(a, ca), (b, cb)] = pair else {
        return Ok(None);
    };
    let (on_a, on_b) = (writes_at(cx, a, *ca), writes_at(cx, b, *cb));
    let mut anchors: Vec<Anchor<'_>> = on_a
        .iter()
        .map(|w| (w, &on_b, b, *cb))
        .chain(on_b.iter().map(|w| (w, &on_a, a, *ca)))
        .collect();
    anchors.sort_by(|x, y| (x.0.change, &x.0.block).cmp(&(y.0.change, &y.0.block)));
    for (w, others, other, oc) in anchors {
        let Some(mine) = write_item(w, if other == b { *ca } else { *cb }) else {
            continue;
        };
        let theirs = match others
            .iter()
            .find(|o| o.change == w.change || cx.writes.common_view(&o.change, &w.change))
        {
            Some(o) => write_item(o, oc),
            None if cx.writes.untouched_in_a_view_of(&w.change, other) => {
                canon_item(cx, other, oc)?
            }
            None => None,
        };
        if let Some(theirs) = theirs {
            let mut items = vec![mine, theirs];
            items.sort();
            return Ok(Some(items));
        }
    }
    if let Some(items) = cross_form(cx, sides, (a, *ca, &on_a), (b, *cb, &on_b))? {
        return Ok(Some(items));
    }
    if !cx.writes.on(a).is_empty() || !cx.writes.on(b).is_empty() {
        return Ok(None);
    }
    canon_pair(cx, (a, *ca), (b, *cb))
}

/// One block of a pair at a content, with its live writes at that content.
type AtContent<'w> = (&'w BlockId, Hash, &'w [Arc<Write>]);

/// The cross-view form of a pair (nb5, see [`ReadState::pair_now`]): the first two live writes,
/// by the change ids in block order, of changes that share no view, each eligible in its own
/// view, whose blocks a tie rule joins. Section and area never make this form.
fn cross_form(
    cx: &DetectCx<'_>,
    sides: &SideScopes,
    x: AtContent<'_>,
    y: AtContent<'_>,
) -> Result<Option<Vec<Item>>, RdyError> {
    if x.0 == y.0 {
        return Ok(None);
    }
    let (x, y) = if x.0 < y.0 { (x, y) } else { (y, x) };
    let st = cx.st;
    for wx in x.2 {
        for wy in y.2 {
            if wx.change == wy.change || cx.writes.common_view(&wx.change, &wy.change) {
                continue;
            }
            let (Some(bx), Some(by)) = (wx.new.as_deref(), wy.new.as_deref()) else {
                continue;
            };
            let (Some(sx), Some(sy)) = (
                sides.of_write(st, cx.writes, wx)?,
                sides.of_write(st, cx.writes, wy)?,
            ) else {
                continue;
            };
            if !side_eligible(st, &sx, bx)? || !side_eligible(st, &sy, by)? {
                continue;
            }
            let mut tied = false;
            for rule in Rule::TIES {
                if exact_holds(st, rule, (bx, &sx), (by, &sy))? {
                    tied = true;
                    break;
                }
            }
            if !tied {
                continue;
            }
            if let (Some(ix), Some(iy)) = (write_item(wx, x.1), write_item(wy, y.1)) {
                let mut items = vec![ix, iy];
                items.sort();
                return Ok(Some(items));
            }
        }
    }
    Ok(None)
}

/// Whether some in-flight change other than `change` shares no view with it.
fn has_cross_change(writes: &Writes, change: &Hash) -> bool {
    let Some(f) = writes.change(change) else {
        return false;
    };
    writes
        .changes()
        .any(|g| g.id != *change && !g.views.iter().any(|v| f.views.contains(v)))
}

/// Whether a link of kind `kind` from a local block to a cited block is a premise: a reference,
/// or a suspect link other than `reflects` (the rule the five triggers read across projects).
fn foreign_premise(kind: &EdgeKind) -> bool {
    match kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => n != "reflects" && SUSPECT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

/// A cited block at its current delivered version, as a pair item with the block it names: the
/// version the current view of its repo holds. `None` when no live source holds the block, the
/// version is redacted, or the delivered facts say it is deprecated, superseded or revoked
/// (a deprecated block never takes part).
fn cited_item(st: &ReadState, target: &ForeignId) -> Result<Option<(Item, Arc<Block>)>, RdyError> {
    let Some(held) = st.foreign()?.current_version(target).cloned() else {
        return Ok(None);
    };
    let v = &held.version;
    let d = &v.derived;
    if d.lifecycle.as_deref() == Some("deprecated")
        || matches!(d.status.as_deref(), Some("superseded" | "revoked"))
        || !d.superseded_by.is_empty()
    {
        return Ok(None);
    }
    let block = st.reader().get_block(&v.version)?;
    if block.is_redacted() || block.id != v.id || !takes_part(st.pack(), &block) {
        return Ok(None);
    }
    let item = Item {
        block: target.id.clone(),
        side: SideRef::Foreign {
            repo: target.repo,
            entry: held.entry,
        },
        version: v.version,
        content: v.content,
    };
    Ok(Some((item, Arc::new(block))))
}

/// The pair key of two blocks at two contents.
fn key_of(pair: &[(BlockId, Hash); 2]) -> Hash {
    pair_key(
        &pair
            .clone()
            .map(|(block, content)| PairItem { block, content }),
    )
}

impl CrossIndex {
    /// The indexes over every live write of `writes`, each visited once: refused before any
    /// index is built when there are more than `limits.writes`, deletes included.
    fn build(
        st: &ReadState,
        writes: &Writes,
        sides: &SideScopes,
        limits: CrossLimits,
    ) -> Result<CrossIndex, RdyError> {
        if writes.live().take(limits.writes.saturating_add(1)).count() > limits.writes {
            return Err(limits.writes_error());
        }
        let mut idx = CrossIndex::default();
        for w in writes.live() {
            CROSS_VIEW_INDEX_VISITS.with(|n| n.set(n.get() + 1));
            let Some(b) = w.new.as_deref() else {
                continue;
            };
            let Some(at) = sides.of_write(st, writes, w)? else {
                continue;
            };
            let entities = at.entities(st, b)?;
            for e in edges_of(b) {
                if premise_edge(&e) {
                    idx.premise_in.entry(e.to).or_default().push(Arc::clone(w));
                }
            }
            for t in described(b) {
                idx.describes.entry(t).or_default().push(Arc::clone(w));
            }
            for e in &entities {
                idx.entities
                    .entry(e.clone())
                    .or_default()
                    .push(Arc::clone(w));
            }
            idx.facts
                .insert((w.change, w.block.clone()), WriteFacts { at, entities });
        }
        Ok(idx)
    }
}

/// The sorted canon items of two blocks, or `None` unless canon holds both at those contents.
fn canon_pair(
    cx: &DetectCx<'_>,
    a: (&BlockId, Hash),
    b: (&BlockId, Hash),
) -> Result<Option<Vec<Item>>, RdyError> {
    match (canon_item(cx, a.0, a.1)?, canon_item(cx, b.0, b.1)?) {
        (Some(x), Some(y)) => {
            let mut items = vec![x, y];
            items.sort();
            Ok(Some(items))
        }
        _ => Ok(None),
    }
}

/// The content `x` has as change `c`'s views hold it: a live write of `x` in a view `c` shares
/// (`c`'s own first; then by change id), else canon's when a view of `c` does not touch `x`.
/// `None` when that write deletes `x`, or canon has no `x`.
fn content_in_view_of(cx: &DetectCx<'_>, c: &Hash, x: &BlockId) -> Result<Option<Hash>, RdyError> {
    let live = cx.writes.live_on(x);
    let shared = live
        .iter()
        .find(|w| w.change == *c)
        .or_else(|| live.iter().find(|w| cx.writes.common_view(&w.change, c)));
    if let Some(w) = shared {
        return Ok(w.new.as_deref().map(|b| cx.content_of(b)));
    }
    if !cx.writes.untouched_in_a_view_of(c, x) {
        return Ok(None);
    }
    Ok(cx.canon_m()?.get(x).map(|b| cx.content_of(b)))
}

/// A block the change writes, at the content its write gives it.
struct Subject {
    block: Arc<Block>,
    content: Hash,
}

/// What one candidate run reads: the in-flight scope, read once.
struct Ctx<'a> {
    cx: DetectCx<'a>,
    m: Arc<Materialized>,
    g: Arc<Graph>,
    lex: Arc<Lexicon>,
    /// The docs in path order; lazy.
    docs: OnceCell<Vec<BlockId>>,
    /// The blocks under a doc, by id; lazy.
    subtrees: RefCell<HashMap<BlockId, Rc<Vec<BlockId>>>>,
    paths: RefCell<HashMap<BlockId, String>>,
    /// The areas of each block read so far (final review M5: the area rule tests every block of
    /// every doc for each subject when an area holds fewer than 20 blocks).
    areas: RefCell<HashMap<BlockId, Rc<Vec<String>>>>,
}

impl<'a> Ctx<'a> {
    fn new(st: &'a ReadState, writes: &'a Writes) -> Result<Ctx<'a>, RdyError> {
        let cx = DetectCx::new(st, writes, canon_index_of(st)?, DetectScope::Inflight);
        Ok(Ctx {
            m: cx.inflight_m()?,
            g: cx.inflight_graph()?,
            lex: cx.lexicon()?,
            cx,
            docs: OnceCell::new(),
            subtrees: RefCell::new(HashMap::new()),
            paths: RefCell::new(HashMap::new()),
            areas: RefCell::new(HashMap::new()),
        })
    }

    /// Whether `b` can be a side: it takes part and is not deprecated.
    fn eligible(&self, b: &Block) -> Result<bool, RdyError> {
        Ok(takes_part(self.cx.st.pack(), b) && !self.cx.deprecated(&b.id)?)
    }

    fn eligible_id(&self, x: &BlockId) -> Result<bool, RdyError> {
        match self.m.get(x) {
            Some(b) => self.eligible(b),
            None => Ok(false),
        }
    }

    /// The entities `b`'s text names.
    fn entities(&self, b: &Block) -> BTreeSet<String> {
        self.lex.mentions(b).into_iter().map(|m| m.entity).collect()
    }

    /// The areas `b` is in, read once per run.
    fn areas(&self, b: &Block) -> Rc<Vec<String>> {
        if let Some(a) = self.areas.borrow().get(&b.id) {
            return Rc::clone(a);
        }
        let a = Rc::new(self.cx.st.areas_in(&self.m, b));
        self.areas.borrow_mut().insert(b.id.clone(), Rc::clone(&a));
        a
    }

    /// Whether `rule` holds between `a` and `b`.
    fn holds(&self, rule: Rule, a: &Block, b: &Block) -> bool {
        match rule {
            Rule::Premise => {
                let joins = |from: &BlockId, to: &BlockId| {
                    self.g
                        .outgoing(from)
                        .iter()
                        .any(|e| e.to == *to && premise_edge(e))
                };
                joins(&a.id, &b.id) || joins(&b.id, &a.id)
            }
            Rule::Describes => {
                let ta = described(a);
                described(b).iter().any(|t| ta.contains(t))
            }
            Rule::Entity => {
                let ea = self.entities(a);
                self.entities(b).iter().any(|e| ea.contains(e))
            }
            Rule::Section => a.parent.is_some() && a.parent == b.parent,
            Rule::Area => {
                let aa = self.areas(a);
                self.areas(b).iter().any(|x| aa.contains(x))
            }
        }
    }

    /// The sort key inside a rule: blocks in `home` first, then by doc path, then by id.
    fn order_key(&self, home: Option<&BlockId>, x: &BlockId) -> (bool, String, BlockId) {
        let doc = self.m.doc_of(x).map(|d| d.id.clone());
        let away = doc.is_none() || doc.as_ref() != home;
        let path = doc.map(|d| self.path_of(&d)).unwrap_or_default();
        (away, path, x.clone())
    }

    fn path_of(&self, doc: &BlockId) -> String {
        self.paths
            .borrow_mut()
            .entry(doc.clone())
            .or_insert_with(|| self.m.path_of(doc).unwrap_or_default())
            .clone()
    }

    fn home(&self, s: &Subject) -> Option<BlockId> {
        self.m.doc_of(&s.block.id).map(|d| d.id.clone())
    }

    /// Every doc, in path order.
    fn docs(&self) -> &[BlockId] {
        self.docs.get_or_init(|| {
            let mut docs: Vec<(String, BlockId)> = self
                .m
                .iter()
                .filter(|b| b.block_type == "doc")
                .map(|b| (self.path_of(&b.id), b.id.clone()))
                .collect();
            docs.sort();
            docs.into_iter().map(|(_, id)| id).collect()
        })
    }

    /// The blocks under `doc`, by id.
    fn subtree(&self, doc: &BlockId) -> Rc<Vec<BlockId>> {
        if let Some(v) = self.subtrees.borrow().get(doc) {
            return Rc::clone(v);
        }
        let mut ids = vec![];
        let mut stack = vec![doc.clone()];
        while let Some(id) = stack.pop() {
            for c in self.m.children(Some(&id)) {
                ids.push(c.id.clone());
                stack.push(c.id.clone());
            }
        }
        ids.sort();
        let v = Rc::new(ids);
        self.subtrees
            .borrow_mut()
            .insert(doc.clone(), Rc::clone(&v));
        v
    }
}

/// The targets of `b`'s `describes` links.
fn described(b: &Block) -> BTreeSet<BlockId> {
    b.links
        .get(decision::DESCRIBES)
        .into_iter()
        .flatten()
        .map(|l| l.target.clone())
        .collect()
}

/// The pairs found so far.
struct Builder<'a, 'c> {
    ctx: &'c Ctx<'a>,
    sides: &'c SideScopes,
    change: Hash,
    out: Vec<Request>,
    seen: BTreeSet<(BlockId, BlockId)>,
}

impl Builder<'_, '_> {
    fn full(&self) -> bool {
        self.out.len() >= MAX_CANDIDATES
    }

    fn wants(&self, s: &BlockId, x: &BlockId) -> bool {
        s != x && !self.seen.contains(&ends(s, x))
    }

    /// Adds the pair (`s`, `x`) under `rule` when it is new, `x` can be a side and both items
    /// resolve.
    fn offer(&mut self, rule: Rule, s: &Subject, x: &BlockId) -> Result<(), RdyError> {
        if !self.wants(&s.block.id, x) || !self.ctx.eligible_id(x)? {
            return Ok(());
        }
        self.push(rule, s, x)
    }

    /// Adds the pair (`s`, `x`) under `rule`, with `x` at the content the change's own views
    /// give it (final ruling 2: canon's where they do not touch it) and the items `pair_now`
    /// names. Skipped when the pair does not resolve, or resolves all-canon.
    fn push(&mut self, rule: Rule, s: &Subject, x: &BlockId) -> Result<(), RdyError> {
        let cx = &self.ctx.cx;
        let Some(theirs) = content_in_view_of(cx, &self.change, x)? else {
            return Ok(());
        };
        let pair = [(s.block.id.clone(), s.content), (x.clone(), theirs)];
        let Some(items) = pair_now_in(cx, self.sides, &pair)? else {
            return Ok(());
        };
        if !items.iter().any(|i| matches!(i.side, SideRef::Change(_))) {
            return Ok(());
        }
        let key = items_key(&items);
        self.seen.insert(ends(&s.block.id, x));
        self.out.push(Request {
            key,
            items,
            rule,
            change: self.change,
        });
        Ok(())
    }

    /// Phase 3 (repo links): the pairs of each subject with the cited blocks it points at (a
    /// reference or a suspect link other than `reflects`, the `premise` rule), each at its
    /// current delivered version, while there is room. Each subject's targets go in edge order,
    /// each once. A pair key counts once over all phases. Nothing is read when no source
    /// delivered.
    fn across(&mut self, subjects: &[Subject]) -> Result<(), RdyError> {
        let cx = &self.ctx.cx;
        if cx.st.foreign()?.is_empty() {
            return Ok(());
        }
        let mut keys: BTreeSet<Hash> = self.out.iter().map(|q| q.key).collect();
        for s in subjects {
            let mut targets: BTreeSet<ForeignId> = BTreeSet::new();
            for e in foreign_edges_of(&s.block) {
                if self.full() {
                    return Ok(());
                }
                if !foreign_premise(&e.kind) || !targets.insert(e.to.clone()) {
                    continue;
                }
                let Some((theirs, _)) = cited_item(cx.st, &e.to)? else {
                    continue;
                };
                let sides = [
                    PairSide::Local(PairItem {
                        block: s.block.id.clone(),
                        content: s.content,
                    }),
                    theirs.pair_side(),
                ];
                let Some(items) = cx.st.pair_now_sides(&sides)? else {
                    continue;
                };
                if !items.iter().any(|i| matches!(i.side, SideRef::Change(_))) {
                    continue;
                }
                let key = items_key(&items);
                if !keys.insert(key) {
                    continue;
                }
                self.out.push(Request {
                    key,
                    items,
                    rule: Rule::Premise,
                    change: self.change,
                });
            }
        }
        Ok(())
    }

    /// Offers `xs` in the order inside a rule: `s`'s doc first, then doc path, then id.
    fn offer_by_doc(
        &mut self,
        rule: Rule,
        s: &Subject,
        xs: BTreeSet<BlockId>,
    ) -> Result<(), RdyError> {
        let home = self.ctx.home(s);
        let mut xs: Vec<BlockId> = xs.into_iter().collect();
        xs.sort_by_cached_key(|x| self.ctx.order_key(home.as_ref(), x));
        for x in xs {
            if self.full() {
                break;
            }
            self.offer(rule, s, &x)?;
        }
        Ok(())
    }

    /// Phase 2 (nb5): the pairs of each subject with live writes of changes that share no view
    /// with this change, by the tie rules, while there is room. Each examined incidence (rule,
    /// subject, other live write) counts toward `limits.visits`, before any filter. Returns the
    /// count.
    fn cross(
        &mut self,
        idx: &CrossIndex,
        limits: CrossLimits,
        subjects: &[Subject],
    ) -> Result<usize, RdyError> {
        let cx = &self.ctx.cx;
        let (st, writes) = (cx.st, cx.writes);
        let mut keys: BTreeSet<Hash> = self.out.iter().map(|q| q.key).collect();
        let mut visits = 0usize;
        for rule in Rule::TIES {
            for s in subjects {
                if self.full() {
                    return Ok(visits);
                }
                let Some(mine) = idx.facts.get(&(self.change, s.block.id.clone())) else {
                    continue;
                };
                if !side_eligible(st, &mine.at, &s.block)? {
                    continue;
                }
                let mut found: Vec<Arc<Write>> = vec![];
                let mut visit = |o: &Arc<Write>| -> Result<(), RdyError> {
                    visits += 1;
                    if visits > limits.visits {
                        return Err(limits.visits_error());
                    }
                    found.push(Arc::clone(o));
                    Ok(())
                };
                match rule {
                    Rule::Premise => {
                        for e in edges_of(&s.block) {
                            if premise_edge(&e) {
                                for o in writes.on(&e.to).iter().filter(|o| o.live) {
                                    visit(o)?;
                                }
                            }
                        }
                        for o in idx.premise_in.get(&s.block.id).into_iter().flatten() {
                            visit(o)?;
                        }
                    }
                    Rule::Describes => {
                        for t in described(&s.block) {
                            for o in idx.describes.get(&t).into_iter().flatten() {
                                visit(o)?;
                            }
                        }
                    }
                    Rule::Entity => {
                        for e in &mine.entities {
                            for o in idx.entities.get(e).into_iter().flatten() {
                                visit(o)?;
                            }
                        }
                    }
                    Rule::Section | Rule::Area => {}
                }
                let home = mine.at.m.doc_of(&s.block.id).map(|d| d.id.clone());
                let mut ranked = vec![];
                for o in found {
                    if o.change == self.change
                        || o.block == s.block.id
                        || writes.common_view(&self.change, &o.change)
                    {
                        continue;
                    }
                    let (Some(ob), Some(theirs)) = (
                        o.new.as_deref(),
                        idx.facts.get(&(o.change, o.block.clone())),
                    ) else {
                        continue;
                    };
                    if !side_eligible(st, &theirs.at, ob)? {
                        continue;
                    }
                    let doc = theirs.at.m.doc_of(&o.block).map(|d| d.id.clone());
                    let away = doc.is_none() || doc != home;
                    let path = doc
                        .and_then(|d| theirs.at.m.path_of(&d))
                        .unwrap_or_default();
                    ranked.push(((away, path, o.block.clone(), o.change), o));
                }
                ranked.sort_by(|x, y| x.0.cmp(&y.0));
                ranked.dedup_by(|x, y| x.0 == y.0);
                for (_, o) in ranked {
                    if self.full() {
                        return Ok(visits);
                    }
                    let Some(ob) = o.new.as_deref() else {
                        continue;
                    };
                    let pair = [
                        (s.block.id.clone(), s.content),
                        (o.block.clone(), cx.content_of(ob)),
                    ];
                    let key = key_of(&pair);
                    if keys.contains(&key) {
                        continue;
                    }
                    let Some(items) = pair_now_in(cx, self.sides, &pair)? else {
                        continue;
                    };
                    if !items.iter().any(|i| matches!(i.side, SideRef::Change(_))) {
                        continue;
                    }
                    keys.insert(key);
                    self.out.push(Request {
                        key,
                        items,
                        rule,
                        change: self.change,
                    });
                }
            }
        }
        Ok(visits)
    }

    fn premise(&mut self, s: &Subject) -> Result<(), RdyError> {
        let g = &self.ctx.g;
        let xs: BTreeSet<BlockId> = g
            .outgoing(&s.block.id)
            .iter()
            .filter(|e| premise_edge(e))
            .map(|e| e.to.clone())
            .chain(
                g.incoming(&s.block.id)
                    .iter()
                    .filter(|e| premise_edge(e))
                    .map(|e| e.from.clone()),
            )
            .collect();
        self.offer_by_doc(Rule::Premise, s, xs)
    }

    fn describes(&mut self, s: &Subject) -> Result<(), RdyError> {
        let g = &self.ctx.g;
        let mut xs = BTreeSet::new();
        for t in described(&s.block) {
            for e in g.incoming(&t) {
                if matches!(&e.kind, EdgeKind::Link(n) if n == decision::DESCRIBES) {
                    xs.insert(e.from.clone());
                }
            }
        }
        self.offer_by_doc(Rule::Describes, s, xs)
    }

    fn entity(&mut self, s: &Subject) -> Result<(), RdyError> {
        let ctx = self.ctx;
        let mine = ctx.entities(&s.block);
        let mut xs = BTreeSet::new();
        for e in &mine {
            let Ok(id) = BlockId::parse(e) else {
                continue;
            };
            for x in ctx.lex.naming(&id) {
                if self.wants(&s.block.id, &x) && ctx.eligible_id(&x)? {
                    xs.insert(x);
                }
            }
        }
        let home = ctx.home(s);
        let mut xs: Vec<BlockId> = xs.into_iter().collect();
        xs.sort_by_cached_key(|x| ctx.order_key(home.as_ref(), x));
        for x in xs {
            if self.full() {
                break;
            }
            // naming is a superset: the text decides
            let names_one = ctx
                .m
                .get(&x)
                .is_some_and(|b| ctx.lex.mentions(b).iter().any(|m| mine.contains(&m.entity)));
            if names_one {
                self.push(Rule::Entity, s, &x)?;
            }
        }
        Ok(())
    }

    fn section(&mut self, s: &Subject) -> Result<(), RdyError> {
        let Some(parent) = &s.block.parent else {
            return Ok(());
        };
        let kids = self.ctx.m.children(Some(parent));
        let Some(at) = kids.iter().position(|k| k.id == s.block.id) else {
            return Ok(());
        };
        let mut xs: Vec<(usize, BlockId)> = kids
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != at)
            .map(|(i, k)| (i.abs_diff(at), k.id.clone()))
            .collect();
        xs.sort();
        for (_, x) in xs {
            if self.full() {
                break;
            }
            self.offer(Rule::Section, s, &x)?;
        }
        Ok(())
    }

    fn area(&mut self, s: &Subject) -> Result<(), RdyError> {
        let ctx = self.ctx;
        let mine = ctx.areas(&s.block);
        if mine.is_empty() {
            return Ok(());
        }
        let home = ctx.home(s);
        let mut docs: Vec<BlockId> = home.iter().cloned().collect();
        docs.extend(
            ctx.docs()
                .iter()
                .filter(|d| Some(*d) != home.as_ref())
                .cloned(),
        );
        for doc in docs {
            for x in ctx.subtree(&doc).iter() {
                if self.full() {
                    return Ok(());
                }
                if !self.wants(&s.block.id, x) {
                    continue;
                }
                let Some(b) = ctx.m.get(x) else {
                    continue;
                };
                if ctx.areas(b).iter().any(|a| mine.contains(a)) && ctx.eligible(b)? {
                    self.push(Rule::Area, s, x)?;
                }
            }
        }
        Ok(())
    }
}

/// The two ends of a pair, in order.
fn ends(a: &BlockId, b: &BlockId) -> (BlockId, BlockId) {
    if a <= b {
        (a.clone(), b.clone())
    } else {
        (b.clone(), a.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Rdy, Session, WriteRequest, WriteResult, parse_ops};
    use rdy_model::Role;
    use serde_json::{Value, json};

    fn agent(v: &str) -> Session {
        Session::agent(format!("agent:{v}"), Some(v.into()), Role::Writer).unwrap()
    }

    fn put(r: &mut Rdy, v: &str, ops: Value) -> WriteResult {
        r.write(
            &agent(v),
            WriteRequest {
                view: v.into(),
                ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write to {v}: {e}"))
    }

    /// Lands view `v` straight through the store.
    fn land(r: &mut Rdy, v: &str) {
        let by = agent("seed").provenance();
        let store = r.store_mut();
        let p = store.propose(v, v, by.clone()).unwrap();
        store
            .land_atomic(rdy_store::LandRequest {
                proposal: p,
                accept: None,
                approver: by,
                alt: None,
                relay: None,
                evidence: None,
                assessment: None,
            })
            .unwrap();
        r.invalidate();
    }

    /// Canon: a section with the term Rebase. Views a, b and c each write claims in it that name
    /// Rebase: one in a, two in b, one in c. The changes, by view.
    fn world() -> (tempfile::TempDir, Rdy, Vec<Hash>) {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let w = put(
            &mut r,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Rebase"}, "body": "Rebase is a term."}
            ]),
        );
        let s = w.created["$s"].clone();
        land(&mut r, "seed");
        let claim =
            |body: &str| json!({"op": "create", "type": "claim", "parent": s, "body": body});
        let mut changes = vec![];
        for (v, bodies) in [
            ("a", vec!["Rebase moves pins."]),
            ("b", vec!["A rebase keeps pins.", "Rebases are safe."]),
            ("c", vec!["Rebase again."]),
        ] {
            let ops: Vec<Value> = bodies.into_iter().map(claim).collect();
            let w = put(&mut r, v, json!(ops));
            changes.push(w.change.parse().unwrap());
        }
        (dir, r, changes)
    }

    fn limits(writes: usize, visits: usize) -> CrossViewContext {
        CrossViewContext {
            limits: CrossLimits { writes, visits },
            ..CrossViewContext::default()
        }
    }

    fn texts(e: RdyError) -> (&'static str, String, String) {
        (e.code, e.message, e.fix)
    }

    #[test]
    fn nb5_cross_view_bounds_are_errors_not_partial_requests() {
        let fix = "close the views you no longer need: rdy view close, or rdy_view with op close";
        assert_eq!(
            texts(CrossLimits::FIXED.writes_error()),
            (
                E_LIMIT,
                "cross-view review exceeds 100000 in-flight writes".into(),
                fix.into()
            )
        );
        assert_eq!(
            texts(CrossLimits::FIXED.visits_error()),
            (
                E_LIMIT,
                "cross-view review exceeds 1000000 candidate visits".into(),
                fix.into()
            )
        );
        let (_d, r, changes) = world();
        let st = r.read();
        let live = st.tensions().unwrap().writes().live().count();
        assert_eq!(live, 4);
        let a = changes[0];
        let want = st.requests_from(&a).unwrap();
        assert_eq!(
            want.len(),
            3,
            "Rebase in a pairs with b's two and c's one: {want:#?}"
        );

        // construction: the context's live writes, once
        let got = st
            .requests_from_with(&a, &mut limits(live, usize::MAX))
            .unwrap();
        assert_eq!(got, want, "at the bound");
        let e = st
            .requests_from_with(&a, &mut limits(live - 1, usize::MAX))
            .unwrap_err();
        assert_eq!(
            texts(e),
            (
                E_LIMIT,
                format!("cross-view review exceeds {} in-flight writes", live - 1),
                fix.into()
            ),
            "the first excess refuses, with no partial set"
        );

        // traversal: one change's candidate incidences, reset for the next change
        let mut cx = CrossViewContext::default();
        st.requests_from_with(&a, &mut cx).unwrap();
        let visits = cx.last_visits;
        assert_eq!(visits, 4, "Rebase's four live mentions, a's own included");
        let got = st
            .requests_from_with(&a, &mut limits(live, visits))
            .unwrap();
        assert_eq!(got, want, "at the bound");
        let e = st
            .requests_from_with(&a, &mut limits(live, visits - 1))
            .unwrap_err();
        assert_eq!(
            texts(e),
            (
                E_LIMIT,
                format!("cross-view review exceeds {} candidate visits", visits - 1),
                fix.into()
            )
        );
        // each change's count starts again: b's two subjects make 8, more than a and c
        let per: Vec<usize> = changes
            .iter()
            .map(|c| {
                st.requests_from_with(c, &mut cx).unwrap();
                cx.last_visits
            })
            .collect();
        assert_eq!(per, vec![4, 8, 4]);
        let mut one = limits(live, 8);
        let before = cross_view_index_visits();
        for c in &changes {
            st.requests_from_with(c, &mut one).unwrap();
        }
        assert_eq!(
            cross_view_index_visits() - before,
            live as u64,
            "the writes bound counts one construction for the whole context"
        );
        let e = st
            .requests_from_with(&changes[1], &mut limits(live, 7))
            .unwrap_err();
        assert_eq!(e.message, "cross-view review exceeds 7 candidate visits");
    }

    #[test]
    fn nb5_queue_walk_reuses_each_selected_views_lexicon() {
        let (_d, r, changes) = world();
        let st = r.read();
        let mut cx = CrossViewContext::default();
        let before = cross_view_index_visits();
        let mut lexicons = vec![];
        for c in &changes {
            let got = st.requests_from_with(c, &mut cx).unwrap();
            assert_eq!(got, st.requests_from(c).unwrap(), "as a standalone call");
            lexicons.push(cx.sides.lexicon_views());
        }
        let views: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        assert!(
            lexicons.iter().all(|l| *l == views),
            "each selected view's lexicon is read once, at construction: {lexicons:?}"
        );
        // three standalone calls build their own index; the shared context builds once
        assert_eq!(
            cross_view_index_visits() - before,
            3 * 4 + 4,
            "the context indexes each live write once"
        );
    }
}
