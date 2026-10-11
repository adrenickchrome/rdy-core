//! Candidate pairs (spec §9.3 step 1, D-KJ71P): for each block an in-flight change writes, the blocks most likely to disagree with it, by five fixed rules, at most 20 per change. Deterministic; settled pairs are left to the queue.
use crate::lexicon::{Lexicon, is_content};
use crate::tensions::index::{DetectCx, Writes, canon_index_of, is_normative, premise_edge};
use crate::tensions::{DetectScope, SideRef};
use crate::{Materialized, RdyError, ReadState};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId, Hash, PairItem, decision, pair_key};
use rdy_schema::Pack;
use serde::Serialize;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

/// The most requests one change makes.
pub const MAX_CANDIDATES: usize = 20;

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

/// One pair a change asks a reviewer to judge. `items` are sorted by block.
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
    /// The in-flight write of `block` that holds `content` (the smallest change id first), as an
    /// item on that change's side. When no in-flight write touches `block`, canon's version when
    /// its content is `content`. Else `None`.
    pub fn item_now(&self, block: &BlockId, content: Hash) -> Result<Option<Item>, RdyError> {
        let set = self.tensions()?;
        let cx = DetectCx::new(
            self,
            set.writes(),
            canon_index_of(self)?,
            DetectScope::Inflight,
        );
        item_now_in(&cx, block, content)
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

    /// The requests change `change` makes, in order: the pairs of each block it writes, by rule
    /// rank, then the subject's id, then closeness; each pair once; at most [`MAX_CANDIDATES`].
    /// A change that is not in flight makes none. Settled pairs are not removed here.
    pub fn requests_from(&self, change: &Hash) -> Result<Vec<Request>, RdyError> {
        let set = self.tensions()?;
        let writes: &Writes = set.writes();
        if writes.change(change).is_none() {
            return Ok(vec![]);
        }
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
            if let Some(item) = item_now_in(&ctx.cx, &block.id, content)? {
                subjects.push(Subject { block, item });
            }
        }
        subjects.sort_by(|a, b| a.block.id.cmp(&b.block.id));
        let mut b = Builder {
            ctx: &ctx,
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
        Ok(b.out)
    }
}

impl Rule {
    const ALL: [Rule; 5] = [
        Rule::Premise,
        Rule::Describes,
        Rule::Entity,
        Rule::Section,
        Rule::Area,
    ];
}

/// The item of `block` for the in-flight write that holds `content`, else canon's.
fn item_now_in(
    cx: &DetectCx<'_>,
    block: &BlockId,
    content: Hash,
) -> Result<Option<Item>, RdyError> {
    for w in cx.writes.live_on(block) {
        if let (Some(b), Some(version)) = (w.new.as_deref(), w.version)
            && cx.content_of(b) == content
        {
            return Ok(Some(Item {
                block: block.clone(),
                side: SideRef::Change(w.change),
                version,
                content,
            }));
        }
    }
    if cx.writes.on(block).is_empty()
        && let Some((side, version)) = cx.canon_side(block)?
        && let Some(b) = cx.canon_m()?.get(block)
        && cx.content_of(b) == content
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

/// A block the change writes, with its item.
struct Subject {
    block: Arc<Block>,
    item: Item,
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

    fn areas(&self, b: &Block) -> Vec<String> {
        self.cx.st.areas_in(&self.m, b)
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

    /// Adds the pair (`s`, `x`) under `rule`, with `x` as it is now.
    fn push(&mut self, rule: Rule, s: &Subject, x: &BlockId) -> Result<(), RdyError> {
        let cx = &self.ctx.cx;
        let (Some((side, Some(version))), Some(block)) = (cx.side_at(x)?, self.ctx.m.get(x)) else {
            return Ok(());
        };
        let theirs = Item {
            block: x.clone(),
            side,
            version,
            content: cx.content_of(block),
        };
        let mut items = vec![s.item.clone(), theirs];
        items.sort();
        let key = pair_key(
            &items
                .iter()
                .map(|i| PairItem {
                    block: i.block.clone(),
                    content: i.content,
                })
                .collect::<Vec<_>>(),
        );
        self.seen.insert(ends(&s.block.id, x));
        self.out.push(Request {
            key,
            items,
            rule,
            change: self.change,
        });
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
