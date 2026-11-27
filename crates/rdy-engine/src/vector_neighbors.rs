//! Vector neighbors (D-Y1WEY): advice about content blocks that share words or links with a
//! covered block and that no deterministic rule ties to it. A vector neighbor of a source is a
//! live content block, not of the supporting class, with a containing doc, that is neither the
//! source nor its section or entity neighbor ([`crate::neighbors`]), and whose score with the
//! source reaches the cut's floor. The score is the one scorer of [`crate::vectors`].
//!
//! The advice never refuses, never adds a consequence, a suspect or a plan item, and stores
//! nothing. A source that a work guard stops gives no rows and counts once in
//! [`Advice::skipped`] (D-0ZPAP); so does an eligible source past the cut's source limit. A
//! source with no seed (D-RY7B5) is complete: no rows, no count. Rows past a source's row cap
//! count nowhere. A briefing's cut ([`AdviceCut::BRIEFING`]) shows each neighbor once, under the
//! source that owns it (D-5BM47, D-5Y64F).
use crate::brief::{effective_lifecycle, own_lifecycle};
use crate::neighbors::{Relation, takes_part};
use crate::tensions::index::{DEPRECATED, ended, is_deprecated};
use crate::vectors::{
    Doc, Guard, Operand, Reasons, Scored, VectorBudget, VectorWeights, rank_rows,
    reasons_of_operands, score_source,
};
use crate::{Materialized, lexicon};
use rdy_graph::Graph;
use rdy_model::{Block, BlockId};
use rdy_schema::Pack;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The cut of one advice call: the lowest score that counts, the most rows each source keeps, the
/// most eligible sources it scores, and whether a neighbor shows once. These are fixed judgements
/// (D-Y1WEY, D-5BM47); a cut is a parameter so a test can name a boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdviceCut {
    /// A row needs an exact score of at least this.
    pub floor: f64,
    /// The most rows one source keeps.
    pub per_source: usize,
    /// The most eligible sources the call scores, in the order given. Later ones count as skipped.
    pub max_sources: usize,
    /// D-5Y64F: each neighbor shows once, under its owner: the complete source where its exact
    /// score is highest, ties by the row order. Ownership comes before the row cap, and a
    /// neighbor its owner does not keep goes to no other source.
    pub once: bool,
}

impl AdviceCut {
    /// A briefing (D-5BM47): score at least 0.25, two rows per source, each neighbor once. A
    /// briefing scores its sources only when it has at most 20 ([`AdviceCut::scores`]); its
    /// caller checks that before it builds any weight.
    pub const BRIEFING: AdviceCut = AdviceCut {
        floor: 0.25,
        per_source: 2,
        max_sources: 20,
        once: true,
    };

    /// A write receipt: score at least 0.25, three rows per source, the first ten sources. A
    /// neighbor of several created blocks shows under each.
    pub const WRITE: AdviceCut = AdviceCut {
        floor: 0.25,
        per_source: 3,
        max_sources: 10,
        once: false,
    };

    /// D-5BM47: whether a briefing with `sources` eligible sources scores them: at most
    /// `max_sources`. A wider one scores none and counts every source as skipped.
    pub const fn scores(&self, sources: usize) -> bool {
        sources <= self.max_sources
    }
}

/// One vector neighbor row: `block` is a vector neighbor of `source`. The score is exact; round it
/// ([`crate::vectors::round3`]) only when it is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct AdviceRow {
    /// The covered block the row is for.
    pub source: BlockId,
    /// The source's place among the sources given, from 0.
    pub source_index: usize,
    /// The neighbor.
    pub block: BlockId,
    /// The neighbor's containing doc.
    pub doc: BlockId,
    /// The exact score.
    pub score: f64,
    /// Up to five shared words, by weight product then word.
    pub words: Vec<String>,
    /// Up to five shared link targets, by weight product then id.
    pub links: Vec<BlockId>,
}

/// The advice for a list of sources.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Advice {
    /// Every kept row of every complete source, sorted together: by exact score, then rows whose
    /// neighbor is in its source's doc first, then the neighbor's doc path, its position in the
    /// doc and its id, then the source's place in the list. Rows of one source keep their own
    /// order, so a caller that wants them per source sorts stably by `source_index`.
    pub rows: Vec<AdviceRow>,
    /// The eligible sources whose rows were not computed: past the cut's source limit, or stopped
    /// by a guard.
    pub skipped: usize,
}

/// What a [`AdviceCut::once`] call charges for each row it keeps until ownership is settled: the
/// row and its neighbor's owner entry. The rows of all sources together keep within the
/// per-source scratch limit ([`crate::vectors::SourceLimits::scratch`], 64 MiB).
const OWNER_RECORD_BYTES: u64 =
    (size_of::<(Scored<'static>, &Block)>() + size_of::<(&BlockId, (usize, f64))>()) as u64;

/// Whether `b` is a source of vector advice: it exists in `m`, is a content block of a class
/// other than supporting, and has a containing doc. Content is the pack's own reading
/// ([`lexicon::is_content`]), never the spelling of a type.
pub fn eligible_source(m: &Materialized, pack: &Pack, b: &Block) -> bool {
    takes_part(pack, b) && m.get(&b.id).is_some() && m.doc_of(&b.id).is_some()
}

/// Whether `b` is a target of vector advice: it is a live (not deprecated) content block of a class
/// other than supporting, with a containing doc.
pub fn eligible_target(m: &Materialized, g: &Graph, pack: &Pack, b: &Block) -> bool {
    lexicon::is_content(pack, b)
        && pack.class_of(&b.block_type) != rdy_schema::TypeClass::Supporting
        && m.get(&b.id).is_some()
        && m.doc_of(&b.id).is_some()
        && !is_deprecated(m, g, &b.id)
}

/// A row's tie-break key once scores are equal (module doc of [`Advice::rows`]). It borrows
/// from the snapshot and shares each doc's path, so a large group of equal scores builds no
/// strings; it orders as the owned key would.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Tie<'m> {
    elsewhere: bool,
    doc_path: Rc<str>,
    position: Vec<(&'m str, &'m BlockId)>,
    block: Cow<'m, BlockId>,
    source_index: usize,
}

/// What one advice call reads about the blocks around its candidates, each read once for the
/// call: an ancestor's block, its containing doc and that doc's path, and whether its lifecycle
/// (its own or inherited) is deprecated; and whether a block type takes part. Candidates share
/// sections and docs, so a call reads these once each, not once per candidate. Every answer
/// equals the uncached read.
struct Reader<'m> {
    m: &'m Materialized,
    ups: RefCell<HashMap<&'m BlockId, Up<'m>>>,
    paths: RefCell<HashMap<&'m BlockId, Rc<str>>>,
    parts: RefCell<Vec<(&'m str, bool)>>,
}

/// One ancestor's reads: `m.get`, `m.doc_of` and the doc's path, and whether
/// `effective_lifecycle` is deprecated.
#[derive(Clone)]
struct Up<'m> {
    block: Option<&'m Block>,
    doc: Option<(&'m Block, Rc<str>)>,
    deprecated: bool,
}

impl<'m> Reader<'m> {
    fn new(m: &'m Materialized) -> Reader<'m> {
        Reader {
            m,
            ups: RefCell::new(HashMap::new()),
            paths: RefCell::new(HashMap::new()),
            parts: RefCell::new(Vec::new()),
        }
    }

    fn up(&self, id: &'m BlockId) -> Up<'m> {
        if let Some(u) = self.ups.borrow().get(id) {
            return u.clone();
        }
        let m = self.m;
        let u = Up {
            block: m.get(id),
            doc: m.doc_of(id).map(|d| (d, self.path(&d.id))),
            deprecated: effective_lifecycle(m, Some(id)).as_deref() == Some(DEPRECATED),
        };
        self.ups.borrow_mut().insert(id, u.clone());
        u
    }

    /// The reads of `b`'s parent; `None` for a block with no parent.
    fn parent(&self, b: &'m Block) -> Option<Up<'m>> {
        b.parent.as_ref().map(|p| self.up(p))
    }

    /// `m.doc_of(&b.id)` and its path, for a block `b` of `m` whose parent's reads are `up`:
    /// `b` when it is a doc, else its parent's doc.
    fn doc_of(&self, b: &'m Block, up: Option<&Up<'m>>) -> Option<(&'m Block, Rc<str>)> {
        if b.block_type == "doc" {
            return Some((b, self.path(&b.id)));
        }
        up.and_then(|u| u.doc.clone())
    }

    /// [`takes_part`], read once per block type.
    fn takes_part(&self, pack: &Pack, b: &'m Block) -> bool {
        let mut parts = self.parts.borrow_mut();
        if let Some((_, part)) = parts.iter().find(|(t, _)| *t == b.block_type) {
            return *part;
        }
        let part = takes_part(pack, b);
        parts.push((&b.block_type, part));
        part
    }

    /// [`eligible_target`] for a block `b` of `m`: the lifecycle `b` lacks is its parent's.
    fn eligible_target(&self, g: &Graph, pack: &Pack, b: &'m Block) -> bool {
        if !self.takes_part(pack, b) {
            return false;
        }
        let up = self.parent(b);
        let has_doc = b.block_type == "doc" || up.as_ref().is_some_and(|u| u.doc.is_some());
        let deprecated = match own_lifecycle(b) {
            Some(l) => l.as_str() == Some(DEPRECATED),
            None => up.as_ref().is_some_and(|u| u.deprecated),
        };
        has_doc && !(deprecated || ended(g, &b.id))
    }

    /// The path of doc `d`, or empty when it has none.
    fn path(&self, d: &'m BlockId) -> Rc<str> {
        let mut paths = self.paths.borrow_mut();
        Rc::clone(
            paths
                .entry(d)
                .or_insert_with(|| Rc::from(self.m.path_of(d).unwrap_or_default())),
        )
    }

    /// The chain of (order key, id) from the child of `doc` down to `b`, whose parent's block
    /// is `parent`. Chains compare in reading order: a block follows its parent and its earlier
    /// siblings.
    fn position(
        &self,
        doc: &BlockId,
        b: &'m Block,
        parent: Option<&'m Block>,
    ) -> Vec<(&'m str, &'m BlockId)> {
        let mut chain = Vec::new();
        let mut cur = Some(b);
        for step in 0..=self.m.len() {
            let Some(b) = cur else { break };
            if b.id == *doc {
                break;
            }
            chain.push((b.order.as_str(), &b.id));
            cur = if step == 0 {
                parent
            } else {
                b.parent.as_ref().and_then(|p| self.up(p).block)
            };
        }
        chain.reverse();
        chain
    }

    /// The tie-break key of neighbor `id` (whose block in `m` is `b`) of the source at
    /// `source_index`, whose doc is `source_doc`.
    fn tie(
        &self,
        source_doc: Option<&BlockId>,
        source_index: usize,
        id: Cow<'m, BlockId>,
        b: Option<&'m Block>,
    ) -> Tie<'m> {
        let up = b.and_then(|b| self.parent(b));
        let doc = b.and_then(|b| self.doc_of(b, up.as_ref()));
        let doc_id = doc.as_ref().map(|(d, _)| &d.id);
        Tie {
            elsewhere: doc_id.is_none() || doc_id != source_doc,
            position: doc_id
                .zip(b)
                .map(|(d, b)| self.position(d, b, up.as_ref().and_then(|u| u.block)))
                .unwrap_or_default(),
            doc_path: doc.map_or_else(|| Rc::from(""), |(_, p)| p),
            block: id,
            source_index,
        }
    }
}

/// [`crate::vectors::reasons`] of `source` and `block`, with the source's vectors read once
/// into `source_vectors` for all its rows.
fn row_reasons<'w>(
    w: &'w VectorWeights,
    source_vectors: &mut Option<Operand<'w>>,
    source: &BlockId,
    block: &BlockId,
) -> Result<Reasons, Guard> {
    let a = match source_vectors {
        Some(a) => a,
        None => source_vectors.insert(Operand::of(w, Doc::Block(source))?),
    };
    Ok(reasons_of_operands(a, &Operand::of(w, Doc::Block(block))?))
}

/// The vector neighbors of `sources`, the covered blocks in reading order.
///
/// Only eligible sources ([`eligible_source`]) count; the first `cut.max_sources` are scored with
/// `w`'s weights and the rest add to [`Advice::skipped`]. A source's candidates are the eligible
/// targets ([`eligible_target`]) that its seeds find (D-RY7B5), minus the source and its section
/// and entity neighbors, which `neighbors` already lists; the exclusion reads local adjacencies
/// and runs no bounded traversal. Each candidate is scored with all its features. A source keeps
/// the `cut.per_source` best rows with an exact score of at least `cut.floor`. With `cut.once`,
/// each candidate first gets one owner (D-5Y64F): the complete source with its highest exact
/// score, ties by the row order. A source then keeps its best rows among the ones it owns, and a
/// row it does not keep goes to no other source. `budget` carries the work guards; a guard drops
/// the source it stops, which then owns nothing, and the next source goes on within what is left.
pub fn vector_neighbors_in(
    m: &Materialized,
    g: &Graph,
    pack: &Pack,
    w: &VectorWeights,
    sources: &[BlockId],
    cut: AdviceCut,
    budget: &mut VectorBudget,
) -> Advice {
    vector_neighbors_except(m, g, pack, w, sources, cut, budget, &|_| false)
}

/// [`vector_neighbors_in`], with no block that `except` names as a candidate: a write receipt
/// (D-Y1WEY) leaves out every block the write creates or changes before a source's row cap.
#[allow(clippy::too_many_arguments)]
pub fn vector_neighbors_except(
    m: &Materialized,
    g: &Graph,
    pack: &Pack,
    w: &VectorWeights,
    sources: &[BlockId],
    cut: AdviceCut,
    budget: &mut VectorBudget,
    except: &dyn Fn(&BlockId) -> bool,
) -> Advice {
    let eligible: Vec<(usize, &Block)> = sources
        .iter()
        .enumerate()
        .filter_map(|(i, id)| m.get(id).map(|b| (i, b)))
        .filter(|(_, b)| eligible_source(m, pack, b))
        .collect();
    let mut skipped = eligible.len().saturating_sub(cut.max_sources);
    let reader = Reader::new(m);
    // the scored sources that finished: each with its place, its block, its doc and its rows (all
    // at or over the floor with `once`, else its best `per_source`)
    type Complete<'m, 'w> = (
        usize,
        &'m Block,
        Option<&'m BlockId>,
        Vec<(Scored<'w>, &'m Block)>,
    );
    let mut complete: Vec<Complete<'_, '_>> = Vec::new();
    let scratch = budget.source_limits().scratch;
    let mut held: u64 = 0;
    for &(index, src) in eligible.iter().take(cut.max_sources) {
        let relation = Relation::new(m, g, pack, src);
        // the blocks `keep` accepts, in the order `score_source` scores them
        let mut kept: Vec<&Block> = Vec::new();
        let keep = |id: &BlockId| {
            *id != src.id
                && !except(id)
                && m.get(id).is_some_and(|b| {
                    let ok = reader.eligible_target(g, pack, b) && !relation.relates(b);
                    if ok {
                        kept.push(b);
                    }
                    ok
                })
        };
        let Ok(scored) = score_source(w, Doc::Block(&src.id), keep, budget) else {
            skipped += 1;
            continue;
        };
        debug_assert_eq!(scored.len(), kept.len());
        let source_doc = m.doc_of(&src.id).map(|d| &d.id);
        let rows: Vec<(Scored<'_>, &Block)> = if cut.once {
            // the rows wait for ownership: charge them before they are kept
            let n = scored.iter().filter(|s| s.score >= cut.floor).count() as u64;
            let need = held.checked_add(n.saturating_mul(OWNER_RECORD_BYTES));
            let Some(need) = need.filter(|v| *v <= scratch) else {
                skipped += 1;
                continue;
            };
            held = need;
            scored
                .into_iter()
                .zip(kept)
                .filter(|(s, _)| s.score >= cut.floor)
                .collect()
        } else {
            rank_rows(
                scored.into_iter().zip(kept).collect(),
                |(s, _)| s.score,
                cut.floor,
                cut.per_source,
                |(_, b)| reader.tie(source_doc, index, Cow::Borrowed(&b.id), Some(b)),
            )
        };
        complete.push((index, src, source_doc, rows));
    }
    if cut.once {
        // D-5Y64F: each candidate's owner, as its place in `complete` and its exact score
        let mut owner: HashMap<&BlockId, (usize, f64)> = HashMap::new();
        for (at, (index, _, doc, rows)) in complete.iter().enumerate() {
            for (s, b) in rows {
                let better = match owner.get(&b.id) {
                    None => true,
                    Some(&(was, score)) => match s.score.total_cmp(&score) {
                        std::cmp::Ordering::Greater => true,
                        std::cmp::Ordering::Less => false,
                        std::cmp::Ordering::Equal => {
                            let (was_index, _, was_doc, _) = &complete[was];
                            reader.tie(*doc, *index, Cow::Borrowed(&b.id), Some(b))
                                < reader.tie(*was_doc, *was_index, Cow::Borrowed(&b.id), Some(b))
                        }
                    },
                };
                if better {
                    owner.insert(&b.id, (at, s.score));
                }
            }
        }
        for (at, (index, _, doc, rows)) in complete.iter_mut().enumerate() {
            let owned: Vec<(Scored<'_>, &Block)> = std::mem::take(rows)
                .into_iter()
                .filter(|(_, b)| owner.get(&b.id).is_some_and(|(o, _)| *o == at))
                .collect();
            *rows = rank_rows(
                owned,
                |(s, _)| s.score,
                cut.floor,
                cut.per_source,
                |(_, b)| reader.tie(*doc, *index, Cow::Borrowed(&b.id), Some(b)),
            );
        }
    }
    let mut rows: Vec<AdviceRow> = Vec::new();
    for (index, src, _, top) in complete {
        let mut mine = Vec::with_capacity(top.len());
        let mut source_vectors = None;
        for (s, _) in top {
            // a source stopped here gives no rows; the rows it owned go to no other source
            let Ok(why) = row_reasons(w, &mut source_vectors, &src.id, s.block) else {
                mine.clear();
                skipped += 1;
                break;
            };
            mine.push(AdviceRow {
                source: src.id.clone(),
                source_index: index,
                block: s.block.clone(),
                doc: m
                    .doc_of(s.block)
                    .map(|d| d.id.clone())
                    .unwrap_or_else(|| s.block.clone()),
                score: s.score,
                words: why.words,
                links: why.links,
            });
        }
        rows.extend(mine);
    }
    let rows = rank_rows(
        rows,
        |r| r.score,
        cut.floor,
        usize::MAX,
        |r| {
            let doc = m.doc_of(&r.source).map(|d| &d.id);
            reader.tie(
                doc,
                r.source_index,
                Cow::Owned(r.block.clone()),
                m.get(&r.block),
            )
        },
    );
    Advice { rows, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::Lexicon;
    use crate::vectors::{
        BudgetUse, LEXICAL_SHARE, LINK_SHARE, MAX_REASON_ITEMS, MAX_SEED_TARGETS, MAX_SEED_WORDS,
        SOURCE_CANDIDATES, SOURCE_POSTINGS, SOURCE_PRODUCTS, Seeds, SourceLimits, WeightScope,
        candidates, idf, reasons as reasons_of, round3, score_operands, score_pair, select_seeds,
        tf,
    };
    use crate::{Rdy, Scope};
    use rdy_model::{CROCKFORD, FieldValue, Link, StateSnapshot};
    use rdy_schema::PackSet;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    fn pack() -> Pack {
        PackSet::with_builtin().resolve("rdy-standard").unwrap()
    }

    /// The `n`th id with `prefix`, in id order for a fixed prefix.
    fn nid(prefix: &str, n: usize) -> BlockId {
        let mut body = [b'0'; 5];
        let mut v = n;
        for d in body.iter_mut().rev() {
            *d = CROCKFORD[v % 32];
            v /= 32;
        }
        BlockId::parse(&format!("{prefix}-{}", std::str::from_utf8(&body).unwrap())).unwrap()
    }

    fn blk(id: &BlockId, ty: &str, parent: Option<&BlockId>, order: &str, body: &str) -> Block {
        Block {
            id: id.clone(),
            block_type: ty.to_string(),
            parent: parent.cloned(),
            order: order.to_string(),
            fields: BTreeMap::new(),
            body: body.to_string(),
            links: Default::default(),
            pins: Default::default(),
            foreign: Default::default(),
            read_identity: None,
        }
    }

    /// A project under construction: docs, sections and claims with ids made in creation order.
    struct Proj {
        blocks: Vec<Block>,
        next: usize,
    }

    impl Proj {
        fn new() -> Proj {
            Proj {
                blocks: vec![],
                next: 0,
            }
        }

        fn id(&mut self, prefix: &str) -> BlockId {
            self.next += 1;
            nid(prefix, self.next)
        }

        fn doc(&mut self, name: &str) -> BlockId {
            let id = self.id("d");
            let mut b = blk(&id, "doc", None, "a1", "");
            b.fields
                .insert("name".to_string(), FieldValue::Str(name.to_string()));
            self.blocks.push(b);
            id
        }

        fn section(&mut self, doc: &BlockId, order: &str) -> BlockId {
            let id = self.id("s");
            self.blocks.push(blk(&id, "section", Some(doc), order, ""));
            id
        }

        fn claim(&mut self, section: &BlockId, order: &str, body: &str) -> BlockId {
            let id = self.id("c");
            self.blocks
                .push(blk(&id, "claim", Some(section), order, body));
            id
        }

        /// A claim with its own section in `doc`.
        fn alone(&mut self, doc: &BlockId, body: &str) -> BlockId {
            let s = self.section(doc, "a1");
            self.claim(&s, "a1", body)
        }

        fn put(&mut self, b: Block) -> BlockId {
            let id = b.id.clone();
            self.blocks.push(b);
            id
        }

        fn edit(&mut self, id: &BlockId, f: impl FnOnce(&mut Block)) {
            f(self.blocks.iter_mut().find(|b| b.id == *id).unwrap());
        }
    }

    /// The scorer's own cut, without a briefing's two-row cap and one owner per neighbor: ten
    /// rows for each of the first 20 sources, at 0.25 or over. Tests of scoring, ties, guards
    /// and stability use it; [`AdviceCut::BRIEFING`] and [`AdviceCut::WRITE`] have their own.
    const SCORER: AdviceCut = AdviceCut {
        floor: 0.25,
        per_source: 10,
        max_sources: 20,
        once: false,
    };

    /// A state built from blocks: its blocks, graph, pack and unnormalized weights.
    struct World {
        m: Materialized,
        g: Graph,
        pack: Pack,
        w: VectorWeights,
    }

    impl World {
        fn new(blocks: &[Block]) -> World {
            let pack = pack();
            let snap = StateSnapshot {
                blocks: blocks
                    .iter()
                    .map(|b| (b.id.clone(), b.version_hash()))
                    .collect(),
            };
            let m = Materialized::build(
                snap,
                blocks.iter().map(|b| (b.id.clone(), b.clone())).collect(),
            );
            World {
                g: Graph::build_from(blocks.iter()),
                w: VectorWeights::of(Arc::new(Lexicon::build(&pack, blocks.iter()))),
                m,
                pack,
            }
        }

        fn advice(&self, sources: &[BlockId], cut: AdviceCut) -> (Advice, BudgetUse) {
            let mut budget = VectorBudget::briefing();
            let a = vector_neighbors_in(
                &self.m,
                &self.g,
                &self.pack,
                &self.w,
                sources,
                cut,
                &mut budget,
            );
            (a, budget.used())
        }
    }

    fn ids(rows: &[AdviceRow]) -> Vec<(BlockId, BlockId)> {
        rows.iter()
            .map(|r| (r.source.clone(), r.block.clone()))
            .collect()
    }

    /// The dot product of two unit vectors, summed in key order, by an independent loop.
    fn manual_dot<K: Ord>(a: &[(K, f64)], b: &[(K, f64)]) -> f64 {
        let mut sum = 0.0;
        for (ka, va) in a {
            if let Ok(i) = b.binary_search_by(|(kb, _)| kb.cmp(ka)) {
                sum += va * b[i].1;
            }
        }
        sum
    }

    // ---- candidate selection (D-RY7B5) ----

    #[derive(Clone, Copy)]
    enum Kind {
        Words,
        Links,
    }

    fn item(kind: Kind, i: usize) -> String {
        match kind {
            Kind::Words => format!("w{i:02}"),
            Kind::Links => nid("e", i).to_string(),
        }
    }

    fn give(b: &mut Block, kind: Kind, i: usize) {
        match kind {
            Kind::Words => {
                b.body.push(' ');
                b.body.push_str(&item(kind, i));
            }
            Kind::Links => {
                let t = nid("e", i);
                b.links
                    .entry("describes".to_string())
                    .or_default()
                    .push(Link {
                        target: t,
                        part: None,
                    });
            }
        }
    }

    /// A source holding items `0..k`, and blocks that reach it: with `ties`, one `t` block and one
    /// `f` block for each item, so every item holds the source and two others and all weigh the
    /// same; without, a `t` block for each item and a `p` block `j` holding items `j..k`, so item
    /// `i` has `i + 3` holders and weighs less than item `i - 1`.
    fn fan(k: usize, kind: Kind, ties: bool) -> Vec<Block> {
        let mk = |id: BlockId| blk(&id, "claim", None, "a1", "");
        let mut blocks = vec![mk(nid("c", 0))];
        for i in 0..k {
            give(&mut blocks[0], kind, i);
        }
        for i in 0..k {
            let mut t = mk(nid("t", i));
            give(&mut t, kind, i);
            blocks.push(t);
            if ties {
                let mut f = mk(nid("f", i));
                give(&mut f, kind, i);
                blocks.push(f);
            }
        }
        if !ties {
            for j in 0..k {
                let mut p = mk(nid("p", j));
                for i in j..k {
                    give(&mut p, kind, i);
                }
                blocks.push(p);
            }
        }
        blocks
    }

    fn found<'a>(w: &'a VectorWeights, budget: &mut VectorBudget) -> (Seeds<'a>, Vec<BlockId>) {
        let op = Operand::of(w, Doc::Block(&nid("c", 0))).unwrap();
        let seeds = select_seeds(w, &op);
        let ids = candidates(w, &seeds, budget)
            .unwrap()
            .into_iter()
            .cloned()
            .collect();
        (seeds, ids)
    }

    #[test]
    fn vxs2_top_64_eligible_seeds_and_postings_union_are_stable() {
        assert_eq!((MAX_SEED_WORDS, MAX_SEED_TARGETS), (64, 64));
        for kind in [Kind::Words, Kind::Links] {
            // 63, 64 and 65 distinct features: only the 64 heaviest find candidates, and a block
            // reachable only through the 65th is not retrieved
            for k in [63usize, 64, 65] {
                let world = World::new(&fan(k, kind, false));
                let mut budget = VectorBudget::briefing();
                let (seeds, got) = found(&world.w, &mut budget);
                let kept = k.min(64);
                let names: Vec<String> = match kind {
                    Kind::Words => seeds.words.iter().map(|s| s.to_string()).collect(),
                    Kind::Links => seeds.targets.iter().map(|t| t.to_string()).collect(),
                };
                // the fewest holders weigh the most
                let want: Vec<String> = (0..kept).map(|i| item(kind, i)).collect();
                assert_eq!(names, want, "{k}: the heaviest first");
                let mut expect: BTreeSet<BlockId> = BTreeSet::new();
                expect.insert(nid("c", 0));
                for i in 0..kept {
                    expect.insert(nid("t", i));
                }
                for j in 0..k {
                    // pad j holds items j..k: reachable through any of them below the cut
                    if j < kept {
                        expect.insert(nid("p", j));
                    }
                }
                assert_eq!(
                    got,
                    expect.iter().cloned().collect::<Vec<_>>(),
                    "{k}: the union, distinct, in id order"
                );
                let used = budget.used();
                let postings: u64 = (0..kept as u64).map(|i| i + 3).sum();
                assert_eq!(used.postings, postings, "{k}: the selected postings only");
                assert_eq!(used.candidates, expect.len() as u64);
                assert_eq!(used.products, 0, "finding candidates multiplies nothing");
                if k == 65 {
                    assert!(!got.contains(&nid("t", 64)) && !got.contains(&nid("p", 64)));
                } else {
                    assert!(got.contains(&nid("t", k - 1)));
                }
            }
            // 66 features of one weight: the cut falls inside the tie, and the words or targets
            // that sort first stay
            let world = World::new(&fan(66, kind, true));
            let mut budget = VectorBudget::briefing();
            let (seeds, got) = found(&world.w, &mut budget);
            let names: Vec<String> = match kind {
                Kind::Words => seeds.words.iter().map(|s| s.to_string()).collect(),
                Kind::Links => seeds.targets.iter().map(|t| t.to_string()).collect(),
            };
            assert_eq!(names, (0..64).map(|i| item(kind, i)).collect::<Vec<_>>());
            let mut expect: BTreeSet<BlockId> = BTreeSet::new();
            expect.insert(nid("c", 0));
            for i in 0..64 {
                expect.insert(nid("t", i));
                expect.insert(nid("f", i));
            }
            assert_eq!(got, expect.into_iter().collect::<Vec<_>>());
            assert_eq!(budget.used().postings, 64 * 3);
            // the same state again gives the same ids, and the same charge
            let mut again = VectorBudget::briefing();
            assert_eq!(found(&world.w, &mut again).1, got);
            assert_eq!(again.used(), budget.used());
        }
    }

    // ---- non-seeds, no-seed sources and the seed boundaries ----

    /// A corpus with `df` holders of one `edge` feature (a word or a link target), each also
    /// holding a feature of its own, and pad blocks that hold only their own, until `n` blocks hold
    /// a feature of the kind. The source is `c-00000`.
    fn edge_world(kind: Kind, df: usize, n: usize) -> World {
        let mut blocks = vec![];
        for i in 0..n {
            let mut b = blk(&nid("c", i), "claim", None, "a1", "");
            if i < df {
                match kind {
                    Kind::Words => b.body.push_str("edge "),
                    Kind::Links => give_target(&mut b, &nid("g", 0)),
                }
            }
            match kind {
                Kind::Words => b.body.push_str(&format!("own{i}")),
                Kind::Links => give_target(&mut b, &nid("h", i)),
            }
            blocks.push(b);
        }
        World::new(&blocks)
    }

    fn give_target(b: &mut Block, to: &BlockId) {
        b.links
            .entry("describes".to_string())
            .or_default()
            .push(Link {
                target: to.clone(),
                part: None,
            });
    }

    #[test]
    fn vxs2_nonseeds_score_and_no_seed_sources_are_complete() {
        // a word that 154 of 160 blocks hold is no seed, yet it counts in every score
        let mut p = Proj::new();
        let da = p.doc("alpha");
        let s = p.alone(&da, "rare common usual");
        let y = p.alone(&da, "rare common usual extra");
        let x = p.alone(&da, "common usual lonely");
        let z = p.alone(&da, "common usual");
        for i in 0..150 {
            p.alone(&da, &format!("common usual filler{i}"));
        }
        let world = World::new(&p.blocks);
        let w = &world.w;
        assert!(w.word_is_seed("rare") && w.word_is_seed("extra"));
        assert!(!w.word_is_seed("common") && !w.word_is_seed("usual"));
        let mut budget = VectorBudget::briefing();
        let got = score_source(w, Doc::Block(&s), |id| *id != s, &mut budget).unwrap();
        assert_eq!(
            got.iter().map(|c| c.block.clone()).collect::<Vec<_>>(),
            vec![y.clone()],
            "only the block that a seed finds; the 152 blocks that share only non-seeds are not"
        );
        // the score has every feature: the non-seed words add to it
        let (ls, ly) = (
            w.lexical(Doc::Block(&s)).unwrap(),
            w.lexical(Doc::Block(&y)).unwrap(),
        );
        let all = manual_dot(&ls, &ly);
        assert_eq!(got[0].score.to_bits(), all.to_bits());
        let rare = |v: &[(&str, f64)]| v.iter().find(|(k, _)| *k == "rare").unwrap().1;
        let seed_only = rare(&ls) * rare(&ly);
        assert!(got[0].score > seed_only, "{} > {seed_only}", got[0].score);
        // a block that shares only non-seeds still has a score; it is just not retrieved
        let mut free = VectorBudget::briefing();
        let sx = score_pair(w, Doc::Block(&s), Doc::Block(&x), &mut free).unwrap();
        assert!(sx > 0.0);
        assert_eq!(free.used().products, 2);
        // a source whose words are all non-seeds is complete: no ids, no charge, no skip
        let mut none = VectorBudget::briefing();
        let r = score_source(w, Doc::Block(&z), |_| true, &mut none).unwrap();
        assert!(r.is_empty());
        assert_eq!(none.used(), BudgetUse::default());
        let (adv, used) = world.advice(std::slice::from_ref(&z), AdviceCut::BRIEFING);
        assert_eq!(
            (adv.rows.len(), adv.skipped, used),
            (0, 0, BudgetUse::default())
        );
        // even with no work left, it is complete and empty, never skipped
        let mut empty = VectorBudget::new(0, 0);
        let adv = vector_neighbors_in(
            &world.m,
            &world.g,
            &world.pack,
            &world.w,
            &[z.clone(), s.clone()],
            AdviceCut::BRIEFING,
            &mut empty,
        );
        assert_eq!(
            (adv.rows.len(), adv.skipped),
            (0, 1),
            "only the seeded source is skipped"
        );

        // both integer seed boundaries, the floor of 100, and the same for link targets
        for kind in [Kind::Words, Kind::Links] {
            let seeds = |df: usize, n: usize| {
                let world = edge_world(kind, df, n);
                let total = match kind {
                    Kind::Words => world.w.corpus_words(),
                    Kind::Links => world.w.corpus_links(),
                };
                assert_eq!(total, n as u64);
                let seed = match kind {
                    Kind::Words => world.w.word_is_seed("edge"),
                    Kind::Links => world.w.target_is_seed(&nid("g", 0)),
                };
                let mut b = VectorBudget::briefing();
                let src = nid("c", 0);
                let ids = score_source(&world.w, Doc::Block(&src), |_| true, &mut b).unwrap();
                (seed, ids.len(), b.used().postings)
            };
            // each block also holds a feature of its own, which seeds as well and finds the
            // block itself: one more posting. The floor: df 100 seeds in any corpus
            assert_eq!(seeds(100, 100), (true, 100, 101));
            assert_eq!(seeds(100, 1_500), (true, 100, 101));
            // above it, a tenth of the corpus: 10 x 101 = 1,010
            assert_eq!(seeds(101, 1_010), (true, 101, 102));
            assert_eq!(
                seeds(101, 1_009),
                (false, 1, 1),
                "101 holders of 1,009 do not seed"
            );
            assert_eq!(seeds(101, 1_011), (true, 101, 102));
            assert_eq!(seeds(102, 1_019), (false, 1, 1));
            assert_eq!(seeds(102, 1_020), (true, 102, 103));
        }
    }

    // ---- numeric words (D-ZDM0G) ----

    /// A corpus of `n` blocks: the first `df` hold `word`, and each holds a word of its own. The
    /// source is `c-00000`.
    fn word_world(word: &str, df: usize, n: usize) -> World {
        let blocks: Vec<Block> = (0..n)
            .map(|i| {
                let body = if i < df {
                    format!("{word} own{i}")
                } else {
                    format!("own{i}")
                };
                blk(&nid("c", i), "claim", None, "a1", &body)
            })
            .collect();
        World::new(&blocks)
    }

    /// The tokenizer's folded words of `text`.
    fn forms(text: &str) -> Vec<String> {
        rdy_model::entity::words(text)
            .into_iter()
            .map(|w| w.folded)
            .collect()
    }

    #[test]
    fn vxs10_numeric_words_never_seed_but_keep_their_weights() {
        // the forms the tokenizer gives: a sign, a comma, a colon and an end dot split a number,
        // and a `.` or `-` between two runs joins them; Unicode digits are word characters
        assert_eq!(
            forms("-5 1,000 12:30 12.5. 2026-11-20 1.2.3 1-2 ٣٤ ５ Ⅻ"),
            [
                "5",
                "1",
                "000",
                "12",
                "30",
                "12.5",
                "2026-11-20",
                "1.2.3",
                "1-2",
                "٣٤",
                "５",
                "ⅻ"
            ]
        );
        assert_eq!(
            forms("v2 5ms x_1 1e5 v1.2.3 a-1 _5 0x1f"),
            ["v2", "5ms", "x_1", "1e5", "v1.2.3", "a-1", "_5", "0x1f"]
        );
        let numeric = [
            "5",
            "000",
            "12.5",
            "2026-11-20",
            "1.2.3",
            "1-2",
            "٣٤",
            "５",
            "ⅻ",
        ];
        let mixed = ["v2", "5ms", "x_1", "1e5", "v1.2.3", "a-1", "_5", "0x1f"];
        // each word in two blocks of 40: every one is under the floor
        let mut blocks = vec![blk(
            &nid("c", 0),
            "claim",
            None,
            "a1",
            &[numeric.join(" "), mixed.join(" ")].join(" "),
        )];
        for (i, word) in numeric.iter().chain(mixed.iter()).enumerate() {
            blocks.push(blk(&nid("t", i), "claim", None, "a1", word));
        }
        for i in 0..(40 - blocks.len()) {
            blocks.push(blk(&nid("p", i), "claim", None, "a1", &format!("pad{i}")));
        }
        let world = World::new(&blocks);
        let w = &world.w;
        assert_eq!(w.corpus_words(), 40);
        for word in numeric {
            assert_eq!(w.df(word), 2, "{word}");
            assert!(!w.word_is_seed(word), "{word} is numeric");
            assert_eq!(
                w.idf(word).unwrap(),
                Some(idf(40, 2)),
                "{word} keeps its idf"
            );
        }
        for word in mixed {
            assert_eq!(w.df(word), 2, "{word}");
            assert!(w.word_is_seed(word), "{word} is mixed");
        }
        // the source keeps every word in its vector; only the mixed words seed
        let op = Operand::of(w, Doc::Block(&nid("c", 0))).unwrap();
        assert_eq!(op.lexical.len(), numeric.len() + mixed.len());
        let seeds = select_seeds(w, &op);
        let mut want: Vec<&str> = mixed.to_vec();
        want.sort_unstable();
        let mut got: Vec<&str> = seeds.words.clone();
        got.sort_unstable();
        assert_eq!(got, want);
        let mut budget = VectorBudget::briefing();
        let found: Vec<BlockId> = score_source(w, Doc::Block(&nid("c", 0)), |_| true, &mut budget)
            .unwrap()
            .into_iter()
            .map(|c| c.block.clone())
            .collect();
        let mut expect: Vec<BlockId> = vec![nid("c", 0)];
        expect.extend((numeric.len()..numeric.len() + mixed.len()).map(|i| nid("t", i)));
        expect.sort();
        assert_eq!(found, expect, "only the blocks a mixed word finds");

        // both frequency boundaries: a numeric word never seeds, a plain word at the same counts
        // does, and the numeric word keeps its weight
        for (df, n) in [(100usize, 100usize), (100, 1_500), (101, 1_010)] {
            let plain = word_world("edge", df, n);
            assert!(plain.w.word_is_seed("edge"), "edge {df}/{n}");
            for word in ["2026-11-20", "12.5", "5"] {
                let world = word_world(word, df, n);
                let w = &world.w;
                assert_eq!((w.df(word), w.corpus_words()), (df as u64, n as u64));
                assert!(!w.word_is_seed(word), "{word} {df}/{n}");
                assert!(w.word_is_seed("own0"));
                let weight = w.idf(word).unwrap().unwrap();
                assert_eq!(weight.to_bits(), idf(n as u64, df as u64).to_bits());
                let raw = [tf(1) * weight, tf(1) * idf(n as u64, 1)];
                let norm = (raw[0] * raw[0] + raw[1] * raw[1]).sqrt();
                let src = nid("c", 0);
                assert_eq!(w.lexical_norm(Doc::Block(&src)).unwrap(), Some(norm));
                let mut b = VectorBudget::briefing();
                let ids = score_source(w, Doc::Block(&src), |_| true, &mut b).unwrap();
                assert_eq!(
                    (ids.len(), b.used().postings),
                    (1, 1),
                    "{word}: only its own word finds the source itself"
                );
            }
        }

        // a pair found through a plain word scores the numeric words it shares
        let mut p = Proj::new();
        let dh = p.doc("home");
        let s = p.alone(&dh, "alpha 2026-11-20 12.5");
        let y = p.alone(&dh, "alpha 2026-11-20 12.5");
        let x = p.alone(&dh, "alpha");
        let z = p.alone(&dh, "beta 2026-11-20 12.5");
        for i in 0..20 {
            p.alone(&dh, &format!("filler{i}"));
        }
        let world = World::new(&p.blocks);
        let w = &world.w;
        assert!(w.word_is_seed("alpha") && !w.word_is_seed("12.5"));
        let mut budget = VectorBudget::briefing();
        let got = score_source(w, Doc::Block(&s), |id| *id != s, &mut budget).unwrap();
        let mut want = vec![y.clone(), x.clone()];
        want.sort();
        assert_eq!(
            got.iter().map(|c| c.block.clone()).collect::<Vec<_>>(),
            want,
            "alpha finds them; the numbers alone do not find z"
        );
        let (ls, ly) = (
            w.lexical(Doc::Block(&s)).unwrap(),
            w.lexical(Doc::Block(&y)).unwrap(),
        );
        let gy = got.iter().find(|c| *c.block == y).unwrap();
        assert_eq!(gy.score.to_bits(), manual_dot(&ls, &ly).to_bits());
        let alpha = |v: &[(&str, f64)]| v.iter().find(|(k, _)| *k == "alpha").unwrap().1;
        assert!(gy.score > alpha(&ls) * alpha(&ly), "the numbers add to it");
        let mut free = VectorBudget::briefing();
        assert!(score_pair(w, Doc::Block(&s), Doc::Block(&z), &mut free).unwrap() > 0.0);

        // a pair found through a link target scores the numeric word it shares
        let (e1, e2) = (nid("e", 1), nid("e", 2));
        let mut a = blk(&nid("c", 1), "claim", None, "a1", "gamma 7.5");
        give_target(&mut a, &e1);
        let mut b = blk(&nid("c", 2), "claim", None, "a1", "delta 7.5");
        give_target(&mut b, &e1);
        let mut c = blk(&nid("c", 3), "claim", None, "a1", "epsilon");
        give_target(&mut c, &e1);
        let mut d = blk(&nid("c", 4), "claim", None, "a1", "zeta");
        give_target(&mut d, &e2);
        let world = World::new(&[a.clone(), b.clone(), c.clone(), d]);
        let w = &world.w;
        assert!(!w.word_is_seed("7.5") && w.target_is_seed(&e1));
        let mut budget = VectorBudget::briefing();
        let got = score_source(w, Doc::Block(&a.id), |id| *id != a.id, &mut budget).unwrap();
        assert_eq!(
            got.iter().map(|c| c.block.clone()).collect::<Vec<_>>(),
            vec![b.id.clone(), c.id.clone()]
        );
        let (la, lb) = (
            w.lexical(Doc::Block(&a.id)).unwrap(),
            w.lexical(Doc::Block(&b.id)).unwrap(),
        );
        let (ka, kb) = (
            w.link(Doc::Block(&a.id)).unwrap(),
            w.link(Doc::Block(&b.id)).unwrap(),
        );
        let lexical = manual_dot(&la, &lb);
        assert!(lexical > 0.0, "7.5 is shared");
        let want = LEXICAL_SHARE * lexical + LINK_SHARE * manual_dot(&ka, &kb);
        assert_eq!(got[0].score.to_bits(), want.to_bits());
        assert!(
            got[0].score > got[1].score,
            "the shared number lifts b over c"
        );
    }

    #[test]
    fn vxs10_numeric_only_sources_are_complete_empty_results() {
        let mut p = Proj::new();
        let dh = p.doc("home");
        let z = p.alone(&dh, "2026-11-20 12.5 5 1.2.3");
        let y = p.alone(&dh, "alpha 2026-11-20 12.5 5");
        let s = p.alone(&dh, "alpha beta");
        for i in 0..20 {
            p.alone(&dh, &format!("filler{i} 5"));
        }
        let world = World::new(&p.blocks);
        let w = &world.w;
        // lexical membership is unchanged: the numbers are words of the lexicon and the vector
        let holders = |word: &str| {
            w.lexicon()
                .word_blocks_of(word)
                .map(|l| l.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let mut both = vec![z.clone(), y.clone()];
        both.sort();
        assert_eq!(holders("2026-11-20"), both);
        assert_eq!(holders("12.5"), both);
        assert_eq!(holders("1.2.3"), vec![z.clone()]);
        assert_eq!(w.df("5"), 22);
        let lz: Vec<&str> = w
            .lexical(Doc::Block(&z))
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(lz, ["1.2.3", "12.5", "2026-11-20", "5"]);
        // the source is complete and empty: no candidate, no charge, no skip
        let mut none = VectorBudget::briefing();
        let r = score_source(w, Doc::Block(&z), |_| true, &mut none).unwrap();
        assert!(r.is_empty());
        assert_eq!(none.used(), BudgetUse::default());
        let (adv, used) = world.advice(std::slice::from_ref(&z), AdviceCut::BRIEFING);
        assert_eq!(
            (adv.rows.len(), adv.skipped, used),
            (0, 0, BudgetUse::default())
        );
        // with no work left it is still complete; only the seeded source is skipped
        let mut empty = VectorBudget::new(0, 0);
        let adv = vector_neighbors_in(
            &world.m,
            &world.g,
            &world.pack,
            &world.w,
            &[z.clone(), s.clone()],
            AdviceCut::BRIEFING,
            &mut empty,
        );
        assert_eq!((adv.rows.len(), adv.skipped), (0, 1));
        // the pair still has a score through the numbers
        let mut free = VectorBudget::briefing();
        assert!(score_pair(w, Doc::Block(&z), Doc::Block(&y), &mut free).unwrap() > 0.0);
    }

    // ---- the briefing shape of the perf workload ----

    #[test]
    fn vxs2_brief_doc_shape_counts_stay_inside_the_section_guard() {
        let dir = tempfile::tempdir().unwrap();
        crate::testkit::generate_tree(dir.path(), 10, 10, 97);
        let r = Rdy::open(dir.path()).unwrap();
        let st = r.read();
        let scope = Scope::View("work".into());
        let (m, g) = (st.materialized(&scope).unwrap(), st.graph(&scope).unwrap());
        let w = st.vector_weights(WeightScope::Read(&scope)).unwrap();
        let pack = st.pack();
        assert_eq!(w.corpus_words(), 9_910, "N");
        assert_eq!(w.corpus_links(), 0, "no link features");
        for word in ["claim", "of"] {
            assert_eq!(w.df(word), 9_700, "{word}");
            assert!(!w.word_is_seed(word));
        }
        assert_eq!(w.df("doc"), 9_800);
        assert!(!w.word_is_seed("doc"));
        for n in 0..10 {
            let word = n.to_string();
            assert_eq!(w.df(&word), 1_070, "number {n}, ten doc titles among them");
            assert!(!w.word_is_seed(&word));
        }
        for n in 10..97 {
            let word = n.to_string();
            assert_eq!(w.df(&word), 100, "number {n}");
            assert!(!w.word_is_seed(&word), "a numeric word never seeds");
        }
        // the briefing's sources: the claims of area-0/doc-0, in reading order
        let doc = m
            .children(None)
            .into_iter()
            .map(|b| b.id.clone())
            .chain((0..m.len()).filter_map(|_| None::<BlockId>))
            .count();
        assert!(doc > 0);
        let doc_id = {
            let mut found = None;
            for id in crate::reader::shape::reading_positions(&m).keys() {
                if m.get(id).is_some_and(|b| b.block_type == "doc")
                    && m.path_of(id).as_deref() == Some("area-0/doc-0")
                {
                    found = Some(id.clone());
                }
            }
            found.expect("area-0/doc-0")
        };
        let sources: Vec<BlockId> = crate::reader::shape::doc_blocks(&m, &doc_id)
            .into_iter()
            .filter(|b| b.block_type == "claim")
            .map(|b| b.id.clone())
            .collect();
        assert_eq!(sources.len(), 97);
        // every claim's words are `claim`, `of`, `doc` and two numbers: no source has a seed, and
        // each charges nothing
        let mut seeded = 0;
        for src in &sources {
            let mut one = VectorBudget::briefing();
            let a = vector_neighbors_in(
                &m,
                &g,
                pack,
                &w,
                std::slice::from_ref(src),
                AdviceCut::BRIEFING,
                &mut one,
            );
            let used = one.used();
            assert_eq!(a.skipped, 0);
            if used == BudgetUse::default() {
                assert!(a.rows.is_empty(), "a source with no seed has no row");
                continue;
            }
            seeded += 1;
        }
        assert_eq!(seeded, 0);
        // the cut scores the first 20 sources, none of them seeded, and skips the other 77
        let mut cut = VectorBudget::briefing();
        let a = vector_neighbors_in(&m, &g, pack, &w, &sources, AdviceCut::BRIEFING, &mut cut);
        assert_eq!(cut.used(), BudgetUse::default());
        assert_eq!((a.skipped, a.rows.len()), (77, 0));
        // the whole section, with no source cut: sums, and nothing near a guard
        let whole = AdviceCut {
            max_sources: sources.len(),
            ..AdviceCut::BRIEFING
        };
        let mut budget = VectorBudget::briefing();
        let a = vector_neighbors_in(&m, &g, pack, &w, &sources, whole, &mut budget);
        let used = budget.used();
        assert_eq!(used, BudgetUse::default());
        assert_eq!(a.skipped, 0, "no guard skips a source");
        assert_eq!(a.rows.len(), 0);
        let l = budget.source_limits();
        assert_eq!(l, SourceLimits::FIXED);
        assert!(used.postings < SOURCE_POSTINGS * 87 && 100 < l.postings);
        assert!(100 < l.candidates && 405 < l.products);
        assert!(used.postings < 2_000_000 && used.products < 8_000_000);
        // the same counts the plan names
        assert_eq!(
            (SOURCE_POSTINGS, SOURCE_CANDIDATES, SOURCE_PRODUCTS),
            (200_000, 50_000, 2_000_000)
        );
    }

    // ---- scores ----

    #[test]
    fn vxs2_score_uses_both_links_or_lexical_fallback() {
        let (e1, e2, e3) = (nid("e", 1), nid("e", 2), nid("e", 3));
        let flat = |i: usize, body: &str| blk(&nid("c", i), "claim", None, "a1", body);
        let a = {
            let mut b = flat(1, "alpha beta");
            give_target(&mut b, &e1);
            give_target(&mut b, &e2);
            b
        };
        let b = {
            let mut b = flat(2, "alpha gamma");
            give_target(&mut b, &e1);
            give_target(&mut b, &e3);
            b
        };
        let c = flat(3, "alpha beta");
        let d = flat(4, "alpha beta");
        let pp = {
            let mut b = flat(5, "pp1");
            give_target(&mut b, &e1);
            b
        };
        let qq = {
            let mut b = flat(6, "qq1");
            give_target(&mut b, &e1);
            b
        };
        let rr = {
            let mut b = flat(7, "rr1");
            give_target(&mut b, &e2);
            b
        };
        let world = World::new(&[a, b, c, d, pp, qq, rr]);
        let w = &world.w;
        let doc = |i: usize| nid("c", i);
        let pair = |x: usize, y: usize| {
            let mut budget = VectorBudget::briefing();
            let s = score_pair(w, Doc::Block(&doc(x)), Doc::Block(&doc(y)), &mut budget).unwrap();
            (s, budget.used().products)
        };
        let lex = |x: usize, y: usize| {
            manual_dot(
                &w.lexical(Doc::Block(&doc(x))).unwrap(),
                &w.lexical(Doc::Block(&doc(y))).unwrap(),
            )
        };
        let lnk = |x: usize, y: usize| {
            manual_dot(
                &w.link(Doc::Block(&doc(x))).unwrap(),
                &w.link(Doc::Block(&doc(y))).unwrap(),
            )
        };
        // both have links: 0.6 x lexical + 0.4 x link
        let (ab, products) = pair(1, 2);
        let want = 0.6 * lex(1, 2) + 0.4 * lnk(1, 2);
        assert_eq!(ab.to_bits(), want.to_bits());
        assert_ne!(ab.to_bits(), lex(1, 2).to_bits());
        assert!(lex(1, 2) > 0.0 && lnk(1, 2) > 0.0);
        assert_eq!(products, 2, "one shared word, one shared target");
        // one side has none: the lexical dot alone, and no link product
        let (ac, products) = pair(1, 3);
        assert_eq!(ac.to_bits(), lex(1, 3).to_bits());
        assert_eq!(products, 2);
        assert_eq!(
            pair(3, 1).0.to_bits(),
            ac.to_bits(),
            "the order of the pair does not matter"
        );
        // neither has links
        let (cd, _) = pair(3, 4);
        assert_eq!(cd.to_bits(), lex(3, 4).to_bits());
        assert!(
            cd <= 1.0 && cd > 0.999_999_999,
            "identical vectors score 1, clamped at most"
        );
        // links only: both have them, no word is shared
        let (pq, _) = pair(5, 6);
        assert_eq!(pq.to_bits(), (0.6 * 0.0 + 0.4 * 1.0_f64).to_bits());
        assert_eq!(pq, 0.4);
        assert_eq!(pair(5, 7).0, 0.0, "different targets and different words");
        // a block with itself: never above 1
        let (aa, _) = pair(1, 1);
        assert!(aa <= 1.0 && aa > 0.999_999_999);
        // a weight read that does not fit is a guard, not a panic
        let tight = VectorWeights::of(Arc::clone(w.lexicon())).with_charge_limit(0);
        let mut budget = VectorBudget::briefing();
        assert_eq!(
            score_pair(
                &tight,
                Doc::Block(&doc(1)),
                Doc::Block(&doc(2)),
                &mut budget
            ),
            Err(Guard::Weights)
        );
        // the operands score the same as the pair
        let (oa, ob) = (
            Operand::of(w, Doc::Block(&doc(1))).unwrap(),
            Operand::of(w, Doc::Block(&doc(2))).unwrap(),
        );
        let mut again = VectorBudget::briefing();
        assert_eq!(
            score_operands(&oa, &ob, &mut again).unwrap().to_bits(),
            ab.to_bits()
        );
    }

    #[test]
    fn vxs2_reasons_keep_five_products_with_stable_ties() {
        assert_eq!(MAX_REASON_ITEMS, 5);
        let words = |order: &str| order.to_string();
        let r = |i: usize| nid("e", i);
        let flat = |i: usize, body: String| blk(&nid("c", i), "claim", None, "a1", &body);
        // eight shared words: two held by 2 blocks, three by 3, three by 4; listed out of order
        let shared = words("m3 f2 r2 m1 f3 r1 m2 f1");
        let mut a = flat(1, shared.clone());
        let mut b = flat(2, shared);
        let mut f1 = flat(3, "m1 m2 m3 f1 f2 f3".to_string());
        let mut f2 = flat(4, "f1 f2 f3".to_string());
        // seven shared targets: two held by 2 blocks, three by 3, two by 4
        for t in [7, 3, 1, 5, 6, 2, 4] {
            give_target(&mut a, &r(t));
            give_target(&mut b, &r(t));
        }
        for t in [3, 4, 5, 6, 7] {
            give_target(&mut f1, &r(t));
        }
        for t in [6, 7] {
            give_target(&mut f2, &r(t));
        }
        let world = World::new(&[a, b, f1, f2]);
        let w = &world.w;
        let get = |x: usize, y: usize| {
            reasons_of(w, Doc::Block(&nid("c", x)), Doc::Block(&nid("c", y))).unwrap()
        };
        let top = get(1, 2);
        assert_eq!(top.words, vec!["r1", "r2", "m1", "m2", "m3"]);
        assert_eq!(top.links, vec![r(1), r(2), r(3), r(4), r(5)]);
        // the rarest products come first: each list is by descending product, ties by item
        let (la, lb) = (
            w.lexical(Doc::Block(&nid("c", 1))).unwrap(),
            w.lexical(Doc::Block(&nid("c", 2))).unwrap(),
        );
        let product = |word: &str| {
            la.iter().find(|(k, _)| *k == word).unwrap().1
                * lb.iter().find(|(k, _)| *k == word).unwrap().1
        };
        assert!(product("r1") == product("r2") && product("r1") > product("m1"));
        assert!(product("m1") == product("m3") && product("m3") > product("f1"));
        // fewer than five shared items: all of them, in the same order; none: empty
        let some = get(1, 4);
        assert_eq!(some.words, vec!["f1", "f2", "f3"]);
        assert_eq!(some.links, vec![r(6), r(7)]);
        assert_eq!(get(2, 1), top, "the order of the pair does not matter");
        let lone = World::new(&[
            blk(&nid("c", 1), "claim", None, "a1", "left"),
            blk(&nid("c", 2), "claim", None, "a1", "right"),
        ]);
        assert_eq!(
            reasons_of(&lone.w, Doc::Block(&nid("c", 1)), Doc::Block(&nid("c", 2))).unwrap(),
            Reasons::default()
        );
    }

    // ---- row order ----

    #[test]
    fn vxs2_exact_score_precedes_doc_position_and_id() {
        // rounded equal scores still sort by their unrounded values, whatever the tie key says
        let rows = vec![("later", 0.300_1_f64, 1u8), ("earlier", 0.300_4, 9)];
        assert_eq!(round3(rows[0].1), round3(rows[1].1));
        let got = rank_rows(rows, |r| r.1, 0.25, 10, |r| r.2);
        assert_eq!(
            got.iter().map(|r| r.0).collect::<Vec<_>>(),
            ["earlier", "later"]
        );
        // the floor takes a score of at least 0.25; the cap keeps the first; the tie key runs only
        // for equal scores and only up to the group that holds the cut
        let calls = std::cell::Cell::new(0);
        let keyed = |r: &(&str, f64, u8)| {
            calls.set(calls.get() + 1);
            r.2
        };
        let rows = vec![
            ("below", f64::from_bits(0.25_f64.to_bits() - 1), 0u8),
            ("floor", 0.25, 5),
            ("tie-b", 0.5, 2),
            ("tie-a", 0.5, 1),
            ("top", 0.9, 7),
            ("tie-c", 0.5, 3),
        ];
        let got = rank_rows(rows.clone(), |r| r.1, 0.25, 10, keyed);
        assert_eq!(
            got.iter().map(|r| r.0).collect::<Vec<_>>(),
            ["top", "tie-a", "tie-b", "tie-c", "floor"]
        );
        assert_eq!(calls.get(), 3, "only the three equal scores are keyed");
        calls.set(0);
        let got = rank_rows(rows.clone(), |r| r.1, 0.25, 1, keyed);
        assert_eq!(got.iter().map(|r| r.0).collect::<Vec<_>>(), ["top"]);
        assert_eq!(calls.get(), 0, "the cut fell before any tie");
        let got = rank_rows(rows, |r| r.1, 0.25, 2, keyed);
        assert_eq!(
            got.iter().map(|r| r.0).collect::<Vec<_>>(),
            ["top", "tie-a"]
        );

        // one source, equal scores: its own doc first, then doc path, then position
        let mut p = Proj::new();
        let (dz, da, dm) = (p.doc("zeta"), p.doc("alpha"), p.doc("mid"));
        let same = "shared words here";
        let s_sec = p.section(&dm, "a1");
        let src = p.claim(&s_sec, "a1", same);
        // two claims in one doc: the later id comes first by position
        let late_sec = p.section(&dm, "a2");
        let early_sec = p.section(&dm, "a0");
        let in_late = p.claim(&late_sec, "a1", same);
        let in_early = p.claim(&early_sec, "a1", same);
        let in_zeta = p.alone(&dz, same);
        let in_alpha = p.alone(&da, same);
        let world = World::new(&p.blocks);
        let (adv, _) = world.advice(std::slice::from_ref(&src), SCORER);
        assert_eq!(
            adv.rows.iter().map(|r| r.block.clone()).collect::<Vec<_>>(),
            vec![in_early, in_late, in_alpha, in_zeta],
            "own doc by position, not by id; then alpha before zeta"
        );
        assert!(
            adv.rows
                .windows(2)
                .all(|r| r[0].score.to_bits() == r[1].score.to_bits())
        );

        // two sources and the global order of a briefing
        let mut p = Proj::new();
        let (a, b, c) = (p.doc("a"), p.doc("b"), p.doc("c"));
        let text = "alpha beta";
        let (sa1, sa2, sa3) = (
            p.section(&a, "a1"),
            p.section(&a, "a2"),
            p.section(&a, "a3"),
        );
        let (sb1, sb2) = (p.section(&b, "a1"), p.section(&b, "a2"));
        let (sc1, sc2) = (p.section(&c, "a1"), p.section(&c, "a2"));
        let s1 = p.claim(&sa1, "a1", text);
        let y = p.claim(&sa2, "a1", text);
        let s2 = p.claim(&sb1, "a1", text);
        let z = p.claim(&sb2, "a1", text);
        let x = p.claim(&sc1, "a1", text);
        let wide = p.claim(&sc2, "a1", "alpha beta gamma");
        let low = p.claim(&sa3, "a1", "alpha beta gamma gamma");
        // six more holders make gamma as common as the words of the text
        for _ in 0..6 {
            p.alone(&c, "gamma");
        }
        let world = World::new(&p.blocks);
        let score = |s: &BlockId, t: &BlockId| {
            let mut budget = VectorBudget::briefing();
            score_pair(&world.w, Doc::Block(s), Doc::Block(t), &mut budget).unwrap()
        };
        assert!(score(&s1, &wide) < score(&s1, &y));
        assert!(
            score(&s1, &low) < score(&s1, &wide),
            "the same-doc block has the lower exact score"
        );
        let (one, _) = world.advice(std::slice::from_ref(&s1), SCORER);
        assert_eq!(
            one.rows.iter().map(|r| r.block.clone()).collect::<Vec<_>>(),
            vec![
                y.clone(),
                s2.clone(),
                z.clone(),
                x.clone(),
                wide.clone(),
                low.clone()
            ]
        );
        let (both, _) = world.advice(&[s1.clone(), s2.clone()], SCORER);
        let pairs = |rows: &[AdviceRow]| ids(rows);
        let sorted = vec![
            (s1.clone(), y.clone()),
            (s2.clone(), z.clone()),
            (s2.clone(), s1.clone()),
            (s2.clone(), y.clone()),
            (s1.clone(), s2.clone()),
            (s1.clone(), z.clone()),
            (s1.clone(), x.clone()),
            (s2.clone(), x.clone()),
            (s1.clone(), wide.clone()),
            (s2.clone(), wide.clone()),
            (s1.clone(), low.clone()),
            (s2.clone(), low.clone()),
        ];
        assert_eq!(pairs(&both.rows), sorted);
        // a row of one source keeps the order it has alone
        let mut by_source = both.rows.clone();
        by_source.sort_by_key(|r| r.source_index);
        let alone: Vec<_> = ids(&one.rows);
        assert_eq!(
            ids(&by_source)
                .into_iter()
                .filter(|(s, _)| *s == s1)
                .collect::<Vec<_>>(),
            alone
        );
        assert_eq!(both.rows[0].source_index, 0);
        assert_eq!(both.rows[1].source_index, 1);
        // the neighbor's doc is on the row
        assert_eq!(both.rows[0].doc, a);
        assert_eq!(both.rows[1].doc, b);
    }

    // ---- the floor, the caps and the exclusions ----

    fn next_up(x: f64) -> f64 {
        f64::from_bits(x.to_bits() + 1)
    }

    #[test]
    fn vxs2_neighbor_floor_caps_and_exclusions_hold() {
        // a score of exactly 0.25: eleven corpus blocks hold a word, and every word is in two of
        // them, so each weighs log2(16/2) = 3 and every norm is a power of two
        let mut p = Proj::new();
        let da = p.doc("alpha");
        let s = p.alone(&da, "a1 a2 a3 a4");
        let x = p.alone(&da, "a1 x2 x3 x4");
        let f1 = p.alone(&da, "a2 a3 a4");
        p.alone(&da, "x2 x3 x4");
        let mut probe = World::new(&p.blocks);
        let n = probe.w.corpus_words() as usize;
        assert!(n <= 15);
        for i in 0..15 - n {
            p.alone(&da, &format!("pad{i}"));
        }
        probe = World::new(&p.blocks);
        assert_eq!(probe.w.corpus_words(), 15);
        assert_eq!(probe.w.idf("a1").unwrap(), Some(3.0));
        let (adv, _) = probe.advice(std::slice::from_ref(&s), AdviceCut::BRIEFING);
        let got: Vec<(BlockId, u64)> = adv
            .rows
            .iter()
            .map(|r| (r.block.clone(), r.score.to_bits()))
            .collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, f1);
        assert_eq!(
            got[1],
            (x.clone(), 0.25_f64.to_bits()),
            "exactly 0.25 stays"
        );
        // just above 0.25 as the floor drops it, 0.25 as the floor keeps it, and a lower floor
        let above = AdviceCut {
            floor: next_up(0.25),
            ..AdviceCut::BRIEFING
        };
        assert_eq!(
            probe.advice(std::slice::from_ref(&s), above).0.rows.len(),
            1
        );
        assert_eq!(adv.rows[1].score, 0.25);
        let below = AdviceCut {
            floor: f64::from_bits(0.25_f64.to_bits() - 1),
            ..AdviceCut::BRIEFING
        };
        assert_eq!(
            probe.advice(std::slice::from_ref(&s), below).0.rows.len(),
            2
        );

        // eleven eligible neighbors: two rows in a briefing, three in a write, none counted as
        // skipped; same-doc rows first
        let mut p = Proj::new();
        let (d_a, d_b) = (p.doc("docs-a"), p.doc("docs-b"));
        let text = "shared seed phrase alpha beta";
        let en = nid("en", 1);
        let mut entity = blk(&en, "entity", Some(&d_a), "b1", "");
        entity
            .fields
            .insert("name".into(), FieldValue::Str("Ledger".into()));
        p.put(entity);
        let src_sec = p.section(&d_a, "a1");
        let src = p.claim(&src_sec, "a1", &format!("{text} {{{{{en}}}}}"));
        let in_a: Vec<BlockId> = (0..6).map(|_| p.alone(&d_a, text)).collect();
        let in_b: Vec<BlockId> = (0..5).map(|_| p.alone(&d_b, text)).collect();
        // would be neighbors by score, and are not
        let sibling = p.claim(&src_sec, "a2", text);
        let tied = {
            let sec = p.section(&d_b, "a1");
            p.claim(&sec, "a1", &format!("{text} {{{{{en}}}}}"))
        };
        let supporting = {
            let sec = p.section(&d_b, "a1");
            let id = p.id("fi");
            p.put(blk(&id, "finding", Some(&sec), "a1", text))
        };
        let deprecated = {
            let id = p.alone(&d_b, text);
            p.edit(&id, |b| {
                b.fields
                    .insert("lifecycle".into(), FieldValue::Str("deprecated".into()));
            });
            id
        };
        let heading = {
            let id = p.id("s");
            let mut b = blk(&id, "section", Some(&d_b), "a1", "");
            b.fields
                .insert("title".into(), FieldValue::Str(text.into()));
            p.put(b)
        };
        let docless = {
            let id = p.id("c");
            p.put(blk(&id, "claim", None, "a1", text))
        };
        let world = World::new(&p.blocks);
        let (m, g, pk) = (&world.m, &world.g, &world.pack);
        let get = |id: &BlockId| m.get(id).unwrap();
        // the predicates, one by one
        for id in [&src, &sibling, &in_a[0], &tied] {
            assert!(eligible_source(m, pk, get(id)));
        }
        assert!(!eligible_source(m, pk, get(&supporting)));
        assert!(!eligible_source(m, pk, get(&heading)));
        assert!(!eligible_source(m, pk, get(&en)));
        assert!(!eligible_source(m, pk, get(&docless)));
        for id in [&src, &in_a[0], &in_b[0], &sibling, &tied] {
            assert!(eligible_target(m, g, pk, get(id)), "{id}");
        }
        for id in [&supporting, &deprecated, &heading, &en, &docless] {
            assert!(!eligible_target(m, g, pk, get(id)), "{id}");
        }
        let rel = Relation::new(m, g, pk, get(&src));
        assert!(rel.relates(get(&sibling)), "a section neighbor");
        assert!(rel.relates(get(&tied)), "an entity neighbor");
        assert!(!rel.relates(get(&src)), "never its own neighbor");
        assert!(!rel.relates(get(&in_a[0])) && !rel.relates(get(&in_b[0])));
        // the rows
        let covered = vec![src.clone()];
        let (adv, _) = world.advice(&covered, AdviceCut::BRIEFING);
        let want: Vec<BlockId> = in_a[..2].to_vec();
        assert_eq!(
            adv.rows.iter().map(|r| r.block.clone()).collect::<Vec<_>>(),
            want
        );
        assert_eq!(adv.skipped, 0, "a row past the cap is not counted");
        assert!(
            adv.rows
                .iter()
                .all(|r| r.source == src && r.source_index == 0)
        );
        let (w3, _) = world.advice(&covered, AdviceCut::WRITE);
        assert_eq!(
            w3.rows.iter().map(|r| r.block.clone()).collect::<Vec<_>>(),
            in_a[..3].to_vec()
        );
        assert_eq!(AdviceCut::BRIEFING.per_source, 2);
        assert_eq!(AdviceCut::WRITE.per_source, 3);
        assert_eq!(
            (
                AdviceCut::BRIEFING.max_sources,
                AdviceCut::WRITE.max_sources
            ),
            (20, 10)
        );
        assert_eq!(
            (AdviceCut::BRIEFING.floor, AdviceCut::WRITE.floor),
            (0.25, 0.25)
        );
        // ineligible sources neither give rows nor count
        let mixed = vec![
            supporting.clone(),
            src.clone(),
            heading.clone(),
            en.clone(),
            nid("zz", 1),
        ];
        let (adv2, _) = world.advice(&mixed, AdviceCut::BRIEFING);
        assert_eq!(ids(&adv2.rows), ids(&adv.rows));
        assert_eq!(adv2.skipped, 0);
        assert!(adv2.rows.iter().all(|r| r.source_index == 1));
        // eligible sources past the limit count once each
        let two = AdviceCut {
            max_sources: 1,
            ..AdviceCut::BRIEFING
        };
        let (adv3, _) = world.advice(&[src.clone(), in_a[0].clone(), in_a[1].clone()], two);
        assert_eq!(adv3.skipped, 2);
        assert!(adv3.rows.iter().all(|r| r.source == src));
        // the reasons: the shared words, no link
        assert_eq!(
            adv.rows[0].words,
            vec!["alpha", "beta", "phrase", "seed", "shared"]
        );
        assert!(adv.rows[0].links.is_empty());
    }

    /// D-5BM47: a briefing scores its sources only when it has at most 20. Twenty sources are
    /// all scored; the briefing checks the count before it builds weights, so 21 score none.
    #[test]
    fn vxs2_briefing_scores_at_most_20_sources() {
        let mut p = Proj::new();
        let da = p.doc("alpha");
        let mut sources = vec![];
        let mut partners = vec![];
        for i in 0..20 {
            sources.push(p.alone(&da, &format!("w{i} x{i}")));
            partners.push(p.alone(&da, &format!("w{i} x{i}")));
        }
        let world = World::new(&p.blocks);
        let (adv, _) = world.advice(&sources, AdviceCut::BRIEFING);
        assert_eq!(adv.skipped, 0);
        let scored: BTreeSet<usize> = adv.rows.iter().map(|r| r.source_index).collect();
        assert_eq!(scored, (0..20).collect::<BTreeSet<usize>>());
        for (s, n) in sources.iter().zip(&partners) {
            assert!(
                adv.rows.iter().any(|r| r.source == *s && r.block == *n),
                "{s} keeps its partner"
            );
        }
        assert_eq!(AdviceCut::BRIEFING.max_sources, 20);
        for (n, scores) in [(0, true), (1, true), (20, true), (21, false), (600, false)] {
            assert_eq!(AdviceCut::BRIEFING.scores(n), scores, "{n}");
        }
    }

    // ---- one owner per target, two rows per source (D-5BM47, D-5Y64F) ----

    /// The exact score of `s` and `t` in `world`.
    fn exact(world: &World, s: &BlockId, t: &BlockId) -> f64 {
        let mut budget = VectorBudget::briefing();
        score_pair(&world.w, Doc::Block(s), Doc::Block(t), &mut budget).unwrap()
    }

    /// The (source, block) pairs of `rows`, as a set.
    fn pair_set(rows: &[AdviceRow]) -> BTreeSet<(BlockId, BlockId)> {
        ids(rows).into_iter().collect()
    }

    #[test]
    fn vxs11_each_target_has_one_highest_scoring_owner() {
        // true ties: S1 and S2 mirror each other's words, so a block with one word of each scores
        // the same exact value with both. Ties go by the row order: a row in its source's doc
        // first, then the source's place.
        let mut p = Proj::new();
        let (da, db, dc) = (p.doc("alpha"), p.doc("beta"), p.doc("gamma"));
        let s1 = p.alone(&dc, "zebra walrus");
        let s2 = p.alone(&da, "otter moose");
        let far = p.alone(&db, "zebra otter");
        let near = p.alone(&da, "zebra otter");
        let world = World::new(&p.blocks);
        for t in [&far, &near] {
            assert_eq!(
                exact(&world, &s1, t).to_bits(),
                exact(&world, &s2, t).to_bits(),
                "a true tie"
            );
        }
        let (adv, _) = world.advice(&[s1.clone(), s2.clone()], AdviceCut::BRIEFING);
        // `far` is in neither source's doc: the first source owns it. `near` is in S2's doc: S2
        // owns it, though S1 comes first.
        assert_eq!(
            pair_set(&adv.rows),
            BTreeSet::from([(s1.clone(), far.clone()), (s2.clone(), near.clone())])
        );
        let (swapped, _) = world.advice(&[s2.clone(), s1.clone()], AdviceCut::BRIEFING);
        assert_eq!(
            pair_set(&swapped.rows),
            BTreeSet::from([(s2.clone(), far.clone()), (s2.clone(), near.clone())]),
            "the first source in the list owns the tie outside both docs"
        );

        // unequal exact scores that round to the same value: the higher exact score owns
        let mut p = Proj::new();
        let (da, db) = (p.doc("alpha"), p.doc("beta"));
        let heavy = |n: usize| {
            let mut words = vec!["kiwi".to_string()];
            words.extend(std::iter::repeat_n("lime".to_string(), n));
            words.join(" ")
        };
        let a = p.alone(&da, &heavy(400));
        let b = p.alone(&da, &heavy(401));
        let t = p.alone(&db, "kiwi lime");
        let world = World::new(&p.blocks);
        let (sa, sb) = (exact(&world, &a, &t), exact(&world, &b, &t));
        assert_ne!(sa.to_bits(), sb.to_bits(), "{sa} {sb}");
        assert_eq!(round3(sa), round3(sb), "{sa} {sb}");
        // the source with the lower exact score comes first, so only the exact score picks
        let (low, high) = if sa < sb { (a, b) } else { (b, a) };
        let (adv, _) = world.advice(&[low.clone(), high.clone()], AdviceCut::BRIEFING);
        let of_t: Vec<&BlockId> = adv
            .rows
            .iter()
            .filter(|r| r.block == t)
            .map(|r| &r.source)
            .collect();
        assert_eq!(of_t, [&high], "the higher exact score owns the target");

        // the cap after ownership: S1 owns three targets and lists its best two; the third is
        // not given to S2, which scores it lower, and S2 lists fewer than two rows
        let mut p = Proj::new();
        let (da, db, dc) = (p.doc("alpha"), p.doc("beta"), p.doc("gamma"));
        let s1 = p.alone(&da, "fig grape plum");
        let s2 = p.alone(&db, "pear kiwi");
        let t1 = p.alone(&dc, "fig grape plum");
        let t2 = p.alone(&dc, "fig grape");
        let t3 = p.alone(&dc, "grape plum pear");
        let t4 = p.alone(&dc, "pear kiwi quince");
        for i in 0..30 {
            p.alone(&dc, &format!("filler{i}"));
        }
        let world = World::new(&p.blocks);
        for t in [&t1, &t2, &t3] {
            assert!(exact(&world, &s1, t) > exact(&world, &s2, t), "{t}");
        }
        assert!(exact(&world, &s1, &t3) < exact(&world, &s1, &t2));
        assert!(exact(&world, &s2, &t3) >= 0.25, "S2 would list t3");
        assert!(exact(&world, &s2, &t4) > exact(&world, &s1, &t4));
        let (adv, _) = world.advice(&[s1.clone(), s2.clone()], AdviceCut::BRIEFING);
        let mine = |s: &BlockId| -> Vec<BlockId> {
            adv.rows
                .iter()
                .filter(|r| r.source == *s)
                .map(|r| r.block.clone())
                .collect()
        };
        assert_eq!(mine(&s1), [t1.clone(), t2.clone()]);
        assert_eq!(
            mine(&s2),
            std::slice::from_ref(&t4),
            "no reassignment; one row is valid"
        );
        assert_eq!(adv.skipped, 0, "rows past the cap count nowhere");

        // a source a guard stops owns nothing: its best target goes to the complete source
        let mut p = Proj::new();
        let (da, db, dc) = (p.doc("alpha"), p.doc("beta"), p.doc("gamma"));
        let s0 = p.alone(&da, "quince bulk");
        let s1 = p.alone(&db, "quince");
        let t = p.alone(&dc, "quince bulk");
        for i in 0..10 {
            p.alone(&dc, &format!("bulk f{i}"));
        }
        let world = World::new(&p.blocks);
        assert!(exact(&world, &s0, &t) > exact(&world, &s1, &t));
        let run = |limits: SourceLimits| {
            let mut budget = VectorBudget::briefing().with_source_limits(limits);
            vector_neighbors_in(
                &world.m,
                &world.g,
                &world.pack,
                &world.w,
                &[s0.clone(), s1.clone()],
                AdviceCut::BRIEFING,
                &mut budget,
            )
        };
        let whole = run(SourceLimits::FIXED);
        assert_eq!(whole.skipped, 0);
        assert!(pair_set(&whole.rows).contains(&(s0.clone(), t.clone())));
        assert!(
            !pair_set(&whole.rows).contains(&(s1.clone(), t.clone())),
            "the stronger complete source owns it"
        );
        let stopped = run(SourceLimits {
            candidates: 5,
            ..SourceLimits::FIXED
        });
        assert_eq!(stopped.skipped, 1, "the stopped source counts once");
        assert!(stopped.rows.iter().all(|r| r.source != s0));
        assert!(pair_set(&stopped.rows).contains(&(s1.clone(), t.clone())));
    }

    #[test]
    fn vxs11_small_brief_caps_and_counters_keep_their_units() {
        // twelve neighbors of one score: two rows, the first two by the row order; the other ten
        // count nowhere. The write cut keeps three.
        let mut p = Proj::new();
        let (da, db) = (p.doc("alpha"), p.doc("beta"));
        let src_sec = p.section(&da, "a1");
        let src = p.claim(&src_sec, "a1", "capword");
        let sec = p.section(&db, "a1");
        let neighbors: Vec<BlockId> = (0..12)
            .map(|k| p.claim(&sec, &format!("a{k:02}"), "capword"))
            .collect();
        // a section neighbor of the source scores 1 too, and is never a row
        let sibling = p.claim(&src_sec, "a2", "capword");
        let world = World::new(&p.blocks);
        let (adv, _) = world.advice(std::slice::from_ref(&src), AdviceCut::BRIEFING);
        assert_eq!(
            adv.rows.iter().map(|r| r.block.clone()).collect::<Vec<_>>(),
            neighbors[..2].to_vec()
        );
        assert_eq!(adv.skipped, 0);
        assert!(
            adv.rows
                .iter()
                .all(|r| r.block != sibling && r.score >= 0.25)
        );
        let (w3, _) = world.advice(std::slice::from_ref(&src), AdviceCut::WRITE);
        assert_eq!(
            w3.rows.iter().map(|r| r.block.clone()).collect::<Vec<_>>(),
            neighbors[..3].to_vec()
        );
        assert_eq!(
            (AdviceCut::BRIEFING.per_source, AdviceCut::BRIEFING.floor),
            (2, 0.25)
        );
        assert_eq!(
            (
                AdviceCut::WRITE.per_source,
                AdviceCut::WRITE.max_sources,
                AdviceCut::WRITE.floor
            ),
            (3, 10, 0.25)
        );

        // the write cut keeps repeats: two created blocks that share neighbors both list them
        let s2 = p.alone(&da, "capword");
        let world = World::new(&p.blocks);
        let (w, _) = world.advice(&[src.clone(), s2.clone()], AdviceCut::WRITE);
        assert_eq!(w.rows.len(), 6, "three rows for each, repeats kept");
        let (b, _) = world.advice(&[src.clone(), s2.clone()], AdviceCut::BRIEFING);
        let blocks: BTreeSet<&BlockId> = b.rows.iter().map(|r| &r.block).collect();
        assert_eq!(blocks.len(), b.rows.len(), "a briefing shows each once");
        assert!(b.rows.len() <= 4);

        // a no-seed source and the row cap add no count; a stopped source counts once
        let mut p = Proj::new();
        let da = p.doc("alpha");
        let common = p.alone(&da, "common common");
        for k in 0..120 {
            p.alone(&da, &format!("common f{k}"));
        }
        let seeded = p.alone(&da, "rare common");
        for _ in 0..3 {
            p.alone(&da, "rare");
        }
        let world = World::new(&p.blocks);
        let (adv, _) = world.advice(&[common.clone(), seeded.clone()], AdviceCut::BRIEFING);
        assert_eq!((adv.rows.len(), adv.skipped), (2, 0));
        let mut none = VectorBudget::new(0, 0);
        let a = vector_neighbors_in(
            &world.m,
            &world.g,
            &world.pack,
            &world.w,
            &[common.clone(), seeded.clone()],
            AdviceCut::BRIEFING,
            &mut none,
        );
        assert_eq!((a.rows.len(), a.skipped), (0, 1));
    }

    // ---- guards ----

    #[test]
    fn vxs2_guards_drop_incomplete_sources_without_refusal() {
        // three sources with group words that 3, 5 and 2 other blocks hold, a no-seed source,
        // and 120 holders of a word that is no seed
        let mut p = Proj::new();
        let da = p.doc("alpha");
        let mut sources = vec![];
        let mut group = |p: &mut Proj, word: &str, n: usize| {
            sources.push(p.alone(&da, &format!("{word} common")));
            for _ in 0..n {
                p.alone(&da, &format!("{word} common"));
            }
        };
        group(&mut p, "g1", 3);
        group(&mut p, "g2", 5);
        group(&mut p, "g3", 2);
        for i in 0..120 {
            p.alone(&da, &format!("common f{i}"));
        }
        let z = p.alone(&da, "common");
        let (s1, s2, s3) = (sources[0].clone(), sources[1].clone(), sources[2].clone());
        let all = vec![s1.clone(), s2.clone(), s3.clone()];
        let run = |blocks: &[Block], budget: &mut VectorBudget, src: &[BlockId]| {
            let world = World::new(blocks);
            let a = vector_neighbors_in(
                &world.m,
                &world.g,
                &world.pack,
                &world.w,
                src,
                SCORER,
                budget,
            );
            (a, world)
        };
        // what each source charges alone: postings, candidates (itself included), pairs, products
        let alone = |s: &BlockId| {
            let mut b = VectorBudget::briefing();
            let (a, _) = run(&p.blocks, &mut b, std::slice::from_ref(s));
            assert_eq!(a.skipped, 0);
            (a.rows.len(), b.used())
        };
        let use_of = |postings, pairs, products| BudgetUse {
            postings,
            candidates: postings,
            pairs,
            products,
        };
        assert_eq!(alone(&s1), (3, use_of(4, 3, 6)));
        assert_eq!(alone(&s2), (5, use_of(6, 5, 10)));
        assert_eq!(alone(&s3), (2, use_of(3, 2, 4)));
        let sources_of =
            |a: &Advice| -> BTreeSet<BlockId> { a.rows.iter().map(|r| r.source.clone()).collect() };
        let set =
            |v: &[&BlockId]| -> BTreeSet<BlockId> { v.iter().map(|b| (*b).clone()).collect() };

        // the whole budget holds all three
        let mut b = VectorBudget::briefing();
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!((a.rows.len(), a.skipped), (10, 0));
        assert_eq!(b.used(), use_of(13, 10, 20));

        // per-source guards stop the one that passes them, and the others go on
        let limits = |f: fn(&mut SourceLimits)| {
            let mut l = SourceLimits::FIXED;
            f(&mut l);
            l
        };
        let lim = limits(|l| l.candidates = 5);
        let mut b = VectorBudget::briefing().with_source_limits(lim);
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!(
            (sources_of(&a), a.skipped, a.rows.len()),
            (set(&[&s1, &s3]), 1, 5)
        );
        let lim = limits(|l| l.postings = 5);
        let mut b = VectorBudget::briefing().with_source_limits(lim);
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!((sources_of(&a), a.skipped), (set(&[&s1, &s3]), 1));
        assert_eq!(
            b.used().postings,
            4 + 3,
            "the stopped source charged no posting"
        );
        let lim = limits(|l| l.products = 9);
        let mut b = VectorBudget::briefing().with_source_limits(lim);
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!((sources_of(&a), a.skipped), (set(&[&s1, &s3]), 1));
        assert_eq!(
            b.used().products,
            6 + 9 + 4,
            "work done before the stop stays charged"
        );
        let lim = limits(|l| l.scratch = 1);
        let mut b = VectorBudget::briefing().with_source_limits(lim);
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!(
            (a.rows.len(), a.skipped),
            (0, 3),
            "no source fits one byte of scratch"
        );

        // whole-budget guards: the source that does not fit is dropped, a later one that does fit
        // still runs
        let mut b = VectorBudget::new(4 + 3, 1_000_000);
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!((sources_of(&a), a.skipped), (set(&[&s1, &s3]), 1));
        assert_eq!(b.used().postings, 7);
        let mut b = VectorBudget::new(1_000_000, 6 + 4);
        let (a, _) = run(&p.blocks, &mut b, &all);
        assert_eq!((sources_of(&a), a.skipped), (set(&[&s1]), 2));
        assert_eq!(
            b.used().products,
            10,
            "the stopped source used what was left"
        );

        // a weight read that does not fit stops the source it belongs to
        let world = World::new(&p.blocks);
        let first = {
            let mut b = VectorBudget::briefing();
            vector_neighbors_in(
                &world.m,
                &world.g,
                &world.pack,
                &world.w,
                std::slice::from_ref(&s1),
                SCORER,
                &mut b,
            );
            world.w.stats().charged
        };
        let tight = VectorWeights::of(Arc::clone(world.w.lexicon())).with_charge_limit(first);
        let mut b = VectorBudget::briefing();
        let a = vector_neighbors_in(
            &world.m,
            &world.g,
            &world.pack,
            &tight,
            &all,
            SCORER,
            &mut b,
        );
        assert_eq!((sources_of(&a), a.skipped), (set(&[&s1]), 2));
        assert!(tight.stats().charged <= first);

        // a no-seed source is complete even with no work left; a seeded one is skipped
        let mut b = VectorBudget::new(0, 0);
        let (a, _) = run(&p.blocks, &mut b, &[z.clone(), s1.clone()]);
        assert_eq!(
            (a.rows.len(), a.skipped, b.used()),
            (0, 1, BudgetUse::default())
        );

        // charges are exact at their limit and refused one past it, and a refusal changes nothing
        let lim = SourceLimits {
            postings: 4,
            candidates: 4,
            products: 4,
            scratch: 4 * 100,
        };
        let mut b = VectorBudget::new(6, 6).with_source_limits(lim);
        b.begin_source();
        assert_eq!(b.charge_postings(4), Ok(()));
        assert_eq!(b.charge_postings(1), Err(Guard::SourcePostings));
        assert_eq!(b.charge_candidates(4, 400), Ok(()));
        assert_eq!(b.charge_candidates(1, 0), Err(Guard::SourceCandidates));
        assert_eq!(b.charge_products(4), Ok(()));
        assert_eq!(b.charge_products(1), Err(Guard::SourceProducts));
        b.begin_source();
        assert_eq!(b.charge_candidates(1, 401), Err(Guard::Scratch));
        assert_eq!(b.charge_postings(3), Err(Guard::SectionPostings));
        assert_eq!(b.charge_postings(2), Ok(()));
        assert_eq!(b.charge_products(3), Err(Guard::SectionProducts));
        assert_eq!(b.charge_products(2), Ok(()));
        let used = b.used();
        assert_eq!((used.postings, used.products, used.candidates), (6, 6, 4));
        // the fixed budgets
        let fixed = [
            (VectorBudget::briefing(), (2_000_000, 8_000_000)),
            (VectorBudget::write(), (100_000, 1_000_000)),
            (VectorBudget::search(), (2_000_000, 8_000_000)),
        ];
        for (mut b, (postings, products)) in fixed {
            assert_eq!(b.source_limits(), SourceLimits::FIXED);
            b.begin_source();
            assert_eq!(b.charge_postings(postings.min(SOURCE_POSTINGS)), Ok(()));
            assert_eq!(b.charge_products(products.min(SOURCE_PRODUCTS)), Ok(()));
        }
        assert_eq!(crate::vectors::SCRATCH_BYTES, 64 * 1024 * 1024);
    }

    // ---- stability ----

    #[test]
    fn vxs2_hash_seed_and_insertion_order_do_not_move_rows() {
        // 24 claims over four docs, 22 words from a small vocabulary, an entity and describes links
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move |n: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as usize) % n
        };
        let vocab: Vec<String> = (0..22).map(|i| format!("term{i:02}")).collect();
        let mut p = Proj::new();
        let docs: Vec<BlockId> = (0..4).map(|i| p.doc(&format!("doc{i}"))).collect();
        let en = nid("en", 1);
        let mut entity = blk(&en, "entity", Some(&docs[0]), "b1", "");
        entity
            .fields
            .insert("name".into(), FieldValue::Str("Ledger".into()));
        p.put(entity);
        let mut sources = vec![];
        for i in 0..24 {
            let body: Vec<&str> = (0..6).map(|_| vocab[next(vocab.len())].as_str()).collect();
            let mut text = body.join(" ");
            if i % 5 == 0 {
                text.push_str(&format!(" {{{{{en}}}}}"));
            }
            let sec = p.section(&docs[i % 4], "a1");
            let c = p.claim(&sec, "a1", &text);
            if i % 3 == 0 {
                let t = nid("e", next(5));
                p.edit(&c, |b| give_target(b, &t));
            }
            if i % 4 == 1 {
                let t = nid("e", next(5));
                p.edit(&c, |b| give_target(b, &t));
            }
            sources.push(c);
        }
        let run = |blocks: &[Block]| {
            let world = World::new(blocks);
            let mut budget = VectorBudget::briefing();
            let a = vector_neighbors_in(
                &world.m,
                &world.g,
                &world.pack,
                &world.w,
                &sources,
                SCORER,
                &mut budget,
            );
            let rows: Vec<_> = a
                .rows
                .iter()
                .map(|r| {
                    (
                        r.source.clone(),
                        r.block.clone(),
                        r.score.to_bits(),
                        r.words.clone(),
                        r.links.clone(),
                        r.doc.clone(),
                    )
                })
                .collect();
            (rows, a.skipped, budget.used())
        };
        let base = run(&p.blocks);
        assert!(base.0.len() > 40, "{} rows", base.0.len());
        // each World builds its lexicon with fresh hash maps, so every run has its own hash seed
        for _ in 0..3 {
            assert_eq!(run(&p.blocks), base);
        }
        let mut reversed = p.blocks.clone();
        reversed.reverse();
        assert_eq!(run(&reversed), base);
        for round in 0..6 {
            let mut shuffled = p.blocks.clone();
            for i in (1..shuffled.len()).rev() {
                shuffled.swap(i, next(i + 1));
            }
            assert_eq!(run(&shuffled), base, "shuffle {round}");
        }
    }
}
