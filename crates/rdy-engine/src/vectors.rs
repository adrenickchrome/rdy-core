//! Model-free vectors (D-ZX5S6): a block's words and link targets, weighted by how rare each is
//! in one weight scope, as unit vectors. The lexicon counts both (`Lexicon::word_counts`,
//! `Lexicon::link_targets`) and carries the counts across writes; this module turns them into
//! weights. Every value is a pure function of the weight scope's state: it uses fixed binary64
//! arithmetic in a fixed order, its own `log2` ([`log2_fixed`]) and no platform logarithm.
//!
//! The weights are lazy. [`VectorWeights`] holds one immutable lexicon and computes each word's
//! idf, each target's weight and each block's norms the first time a read asks for them, once.
//! A read scope keeps its weights on its build, so a write drops only that state's values and a
//! held state keeps its own. Review reads canon plus the one in-flight source block
//! ([`WeightScope::Review`]), which counts as one more corpus block.
//!
//! The scorer comes after the weights: [`select_seeds`] and [`candidates`] find a source's
//! candidates (D-RY7B5), [`score_operands`] scores a pair with every feature (D-ZX5S6),
//! [`rank_rows`] orders and cuts rows by their exact scores, and [`reasons`] names what a pair
//! shares. [`VectorBudget`] holds the work guards. Advice, review and search all use these.
use crate::Scope;
use crate::lexicon::Lexicon;
use rdy_model::{Block, BlockId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// The binary64 bits of ln 2 rounded to nearest (0.6931471805599453): the divisor that turns
/// the series of [`log2_fixed`] into a base-2 logarithm.
pub const LN2_BITS: u64 = 0x3fe6_2e42_fefa_39ef;

/// The number of odd terms [`log2_fixed`] sums: `k = 0..40`.
pub const LOG2_TERMS: u32 = 40;

/// D-RY7B5: a feature with a document frequency up to this always seeds.
pub const SEED_FLOOR: u64 = 100;

/// D-RY7B5: a feature also seeds when this many times its document frequency is at most the
/// corpus size.
pub const SEED_SHARE: u64 = 10;

/// The charged payload one weight state may add, in bytes: 256 MiB. A read that needs more is
/// refused with [`WeightLimit`] and installs nothing.
pub const WEIGHT_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// What one cached word weight (an idf) is charged beside the word's bytes: its key and value.
pub const WORD_ENTRY_BYTES: usize = size_of::<String>() + size_of::<f64>();

/// What one cached block or target value (a norm or a target weight) is charged beside the id's
/// bytes: its key and value.
pub const BLOCK_ENTRY_BYTES: usize = size_of::<BlockId>() + size_of::<f64>();

/// `log2(x)` for a finite `x > 0`, in binary64, the same bits on every platform. Any other `x`
/// gives NaN; the weights never pass one.
///
/// The reduction is fixed: `x = 2^e × m` with `1 ≤ m < 2`, `e` and `m` read from the exponent
/// and fraction bits (a subnormal is first scaled by `2^54`, which is exact). Then
/// `z = (m - 1) / (m + 1)`, and `ln m = 2 × (z + z³/3 + z⁵/5 + …)`. The sum takes the
/// [`LOG2_TERMS`] odd terms `z^(2k+1) / (2k+1)` for `k = 0..40` in ascending order, each power
/// the one before times `z × z`; with `z ≤ 1/3` the last term is below `2^-120` of the first.
/// The result is `e + 2 × sum / ln2`, with `ln2` the binary64 of [`LN2_BITS`]. Every step is
/// one IEEE 754 operation rounded to nearest: no fused multiply-add, no platform logarithm, no
/// reordered sum. A power of two gives exactly `e`.
pub fn log2_fixed(x: f64) -> f64 {
    if !(x.is_finite() && x > 0.0) {
        return f64::NAN;
    }
    let mut bits = x.to_bits();
    let mut biased = ((bits >> 52) & 0x7ff) as i64;
    if biased == 0 {
        // a subnormal: 2^54 times it is normal, and the product is exact
        bits = (x * f64::from_bits(0x4350_0000_0000_0000)).to_bits();
        biased = ((bits >> 52) & 0x7ff) as i64 - 54;
    }
    let e = biased - 1023;
    let m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    let z = (m - 1.0) / (m + 1.0);
    let z2 = z * z;
    let mut sum = 0.0;
    let mut power = z;
    for k in 0..LOG2_TERMS {
        sum += power / f64::from(2 * k + 1);
        power *= z2;
    }
    e as f64 + 2.0 * sum / f64::from_bits(LN2_BITS)
}

/// D-RY7B5: whether a feature that `df` corpus blocks hold seeds candidates in a corpus of `n`
/// blocks: `df <= 100` or `10 × df <= n`, in integers. A feature that does not seed still counts
/// in weights, norms and scores.
pub fn is_seed(df: u64, n: u64) -> bool {
    df <= SEED_FLOOR || df.checked_mul(SEED_SHARE).is_some_and(|x| x <= n)
}

/// The term weight of a word that occurs `count` times in a block: `1 + log2(count)`.
pub fn tf(count: u64) -> f64 {
    if count == 1 {
        // log2_fixed(1.0) is exactly 0, so this is the same bits without the series
        return 1.0;
    }
    1.0 + log2_fixed(count as f64)
}

/// The rarity weight of a feature that `df` of `n` corpus blocks hold: `log2((n + 1) / df)`,
/// the division one binary64 operation. The idf of a word and the weight of a link target.
pub fn idf(n: u64, df: u64) -> f64 {
    log2_fixed((n + 1) as f64 / df as f64)
}

/// The Euclidean length of `weights`, summed in the order given: the square root of the sum of
/// their squares.
fn length(weights: impl Iterator<Item = f64>) -> f64 {
    let mut sum = 0.0;
    for w in weights {
        sum += w * w;
    }
    sum.sqrt()
}

/// How often each word occurs in one block's text, sorted by word. The words are those the
/// lexicon reads (`[\w]+([-.][\w]+)*`, lower-cased): no plural fold and no stop list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordCounts(Arc<[(String, u64)]>);

impl WordCounts {
    /// The counts of `words`, each occurrence once.
    pub(crate) fn of(words: impl IntoIterator<Item = String>) -> WordCounts {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for w in words {
            *counts.entry(w).or_insert(0) += 1;
        }
        WordCounts(counts.into_iter().collect())
    }

    /// Whether the block holds no word.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many distinct words the block holds.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Each word and its count, by word.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0.iter().map(|(w, n)| (w.as_str(), *n))
    }

    /// How often the block holds `word`; 0 when it does not.
    pub fn get(&self, word: &str) -> u64 {
        self.0
            .binary_search_by(|(w, _)| w.as_str().cmp(word))
            .map_or(0, |i| self.0[i].1)
    }
}

/// A block's distinct link targets, sorted: each target of its references and links once,
/// whatever the edges' kinds; never its parent or its children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTargets(Arc<[BlockId]>);

impl LinkTargets {
    /// The targets of `targets`, sorted and distinct already.
    pub(crate) fn of(targets: std::collections::BTreeSet<BlockId>) -> LinkTargets {
        LinkTargets(targets.into_iter().collect())
    }

    /// Whether the block has no target.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many targets the block has.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Each target, by id.
    pub fn iter(&self) -> impl Iterator<Item = &BlockId> {
        self.0.iter()
    }

    /// Whether `target` is one of them.
    pub fn contains(&self, target: &BlockId) -> bool {
        self.0.binary_search(target).is_ok()
    }
}

/// The corpus a read's weights come from (design 7.5).
#[derive(Clone, Copy, Debug)]
pub enum WeightScope<'a> {
    /// The scope the read reads: canon, a view, a landing or a proposal version. Its weights
    /// live on its build.
    Read(&'a Scope),
    /// Review (D-R2CDN): canon at its current landing plus the one in-flight source block,
    /// which counts as one more corpus block even when canon holds a block with its id. No other
    /// view takes part. Its weights are made for the call and never cached under the block's id.
    Review(&'a Block),
}

/// A document of a weight state: a block of the state's lexicon, or the overlay's source block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doc<'a> {
    /// A block of the lexicon, by id; under an overlay, canon's block with that id.
    Block(&'a BlockId),
    /// The overlay's in-flight source block; empty in a state without one.
    Source,
}

/// A lazy weight read could not finish inside [`WEIGHT_CACHE_BYTES`] (or the state's own limit):
/// it installed nothing. Advice skips the source; review keeps only complete sources; ranked
/// search returns its vector-work refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeightLimit;

/// What a weight state has computed so far: the idf entries, the target weights, the lexical
/// and link norms, and the bytes they are charged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WeightStats {
    pub idf: usize,
    pub targets: usize,
    pub lexical_norms: usize,
    pub link_norms: usize,
    pub charged: usize,
}

/// The overlay's source block: its id, words and targets.
struct Source {
    id: BlockId,
    counts: Option<WordCounts>,
    targets: Option<LinkTargets>,
}

/// The lazy values of a weight state, and their charge.
#[derive(Default)]
struct Cache {
    idf: BTreeMap<String, f64>,
    targets: BTreeMap<BlockId, f64>,
    lexical: BTreeMap<BlockId, f64>,
    link: BTreeMap<BlockId, f64>,
    source_lexical: Option<f64>,
    source_link: Option<f64>,
    charged: usize,
}

impl Cache {
    /// Charges `bytes`, or refuses and charges nothing when the total would pass `limit`.
    fn charge(&mut self, bytes: usize, limit: usize) -> Result<(), WeightLimit> {
        match self.charged.checked_add(bytes) {
            Some(total) if total <= limit => {
                self.charged = total;
                Ok(())
            }
            _ => Err(WeightLimit),
        }
    }
}

/// Which of a document's two vectors.
#[derive(Clone, Copy)]
enum Kind {
    Lexical,
    Link,
}

/// The weights of one weight state: its lexicon (and, for review, the source block), and the
/// values computed from them so far. Every value is computed on first read, once, and charged
/// before it is stored; a read that would pass the limit stores nothing.
pub struct VectorWeights {
    lexicon: Arc<Lexicon>,
    source: Option<Source>,
    limit: usize,
    cache: Mutex<Cache>,
}

impl VectorWeights {
    /// The weights of `lexicon`'s blocks, nothing computed yet.
    pub fn of(lexicon: Arc<Lexicon>) -> VectorWeights {
        VectorWeights {
            lexicon,
            source: None,
            limit: WEIGHT_CACHE_BYTES,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Review's weights (D-R2CDN): `canon`'s blocks plus `source`, counted as one more corpus
    /// block even when canon holds a block with its id. `source`'s words and targets are read
    /// the lexicon's way. Nothing computed yet.
    pub fn review(canon: Arc<Lexicon>, source: &Block) -> VectorWeights {
        let (counts, targets) = canon.features_of(source);
        let mut w = VectorWeights::of(canon);
        w.source = Some(Source {
            id: source.id.clone(),
            counts: Some(counts).filter(|c| !c.is_empty()),
            targets: Some(targets).filter(|t| !t.is_empty()),
        });
        w
    }

    /// Test hook: these weights with a charge limit of `bytes` instead of
    /// [`WEIGHT_CACHE_BYTES`].
    #[doc(hidden)]
    pub fn with_charge_limit(mut self, bytes: usize) -> VectorWeights {
        self.limit = bytes;
        self
    }

    /// The charge limit, in bytes.
    pub fn charge_limit(&self) -> usize {
        self.limit
    }

    /// The lexicon the weights read.
    pub fn lexicon(&self) -> &Arc<Lexicon> {
        &self.lexicon
    }

    /// The overlay's source block, for review's weights.
    pub fn source(&self) -> Option<&BlockId> {
        self.source.as_ref().map(|s| &s.id)
    }

    /// `N`: the corpus blocks that hold a word, the overlay's source included.
    pub fn corpus_words(&self) -> u64 {
        let extra = self.source.as_ref().is_some_and(|s| s.counts.is_some());
        self.lexicon.word_blocks() as u64 + u64::from(extra)
    }

    /// `L`: the corpus blocks that have a link target, the overlay's source included.
    pub fn corpus_links(&self) -> u64 {
        let extra = self.source.as_ref().is_some_and(|s| s.targets.is_some());
        self.lexicon.link_blocks() as u64 + u64::from(extra)
    }

    /// `df(word)`: the corpus blocks that hold `word`; 0 for a word no block holds.
    pub fn df(&self, word: &str) -> u64 {
        let extra = self
            .source
            .as_ref()
            .and_then(|s| s.counts.as_ref())
            .is_some_and(|c| c.get(word) > 0);
        self.lexicon.word_df(word) as u64 + u64::from(extra)
    }

    /// `indeg(target)`: the corpus blocks that have `target` as a link target.
    pub fn indeg(&self, target: &BlockId) -> u64 {
        let extra = self
            .source
            .as_ref()
            .and_then(|s| s.targets.as_ref())
            .is_some_and(|t| t.contains(target));
        self.lexicon.indeg(target) as u64 + u64::from(extra)
    }

    /// D-RY7B5: whether `word` is a corpus word that seeds candidates ([`is_seed`] of `df` and
    /// `N`).
    pub fn word_is_seed(&self, word: &str) -> bool {
        let df = self.df(word);
        df > 0 && is_seed(df, self.corpus_words())
    }

    /// D-RY7B5: whether `target` is a corpus target that seeds candidates ([`is_seed`] of
    /// `indeg` and `L`).
    pub fn target_is_seed(&self, target: &BlockId) -> bool {
        let indeg = self.indeg(target);
        indeg > 0 && is_seed(indeg, self.corpus_links())
    }

    /// The words of `doc` and their counts; `None` for a document with no word.
    pub fn counts(&self, doc: Doc<'_>) -> Option<&WordCounts> {
        match doc {
            Doc::Block(id) => self.lexicon.word_counts(id),
            Doc::Source => self.source.as_ref().and_then(|s| s.counts.as_ref()),
        }
    }

    /// The link targets of `doc`; `None` for a document with no target.
    pub fn targets(&self, doc: Doc<'_>) -> Option<&LinkTargets> {
        match doc {
            Doc::Block(id) => self.lexicon.link_targets(id),
            Doc::Source => self.source.as_ref().and_then(|s| s.targets.as_ref()),
        }
    }

    /// What these weights have computed so far.
    pub fn stats(&self) -> WeightStats {
        let c = self.lock();
        WeightStats {
            idf: c.idf.len(),
            targets: c.targets.len(),
            lexical_norms: c.lexical.len() + usize::from(c.source_lexical.is_some()),
            link_norms: c.link.len() + usize::from(c.source_link.is_some()),
            charged: c.charged,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `idf(word) = log2((N + 1) / df(word))`, computed once; `None` for a word no corpus block
    /// holds (it drops out of a query).
    pub fn idf(&self, word: &str) -> Result<Option<f64>, WeightLimit> {
        let df = self.df(word);
        if df == 0 {
            return Ok(None);
        }
        let mut c = self.lock();
        if let Some(v) = c.idf.get(word) {
            return Ok(Some(*v));
        }
        c.charge(WORD_ENTRY_BYTES + word.len(), self.limit)?;
        let v = idf(self.corpus_words(), df);
        c.idf.insert(word.to_string(), v);
        Ok(Some(v))
    }

    /// The weight of link target `target`, `log2((L + 1) / indeg(target))`, computed once;
    /// `None` for a target no corpus block has.
    pub fn target_weight(&self, target: &BlockId) -> Result<Option<f64>, WeightLimit> {
        let indeg = self.indeg(target);
        if indeg == 0 {
            return Ok(None);
        }
        let mut c = self.lock();
        if let Some(v) = c.targets.get(target) {
            return Ok(Some(*v));
        }
        c.charge(BLOCK_ENTRY_BYTES + target.as_str().len(), self.limit)?;
        let v = idf(self.corpus_links(), indeg);
        c.targets.insert(target.clone(), v);
        Ok(Some(v))
    }

    /// The Euclidean length of `doc`'s raw lexical weights (`tf × idf`, summed by word),
    /// computed once with the idf of each of its words; `None` for a document with no word.
    pub fn lexical_norm(&self, doc: Doc<'_>) -> Result<Option<f64>, WeightLimit> {
        self.norm(doc, Kind::Lexical)
    }

    /// The Euclidean length of `doc`'s link weights (summed by target), computed once with the
    /// weight of each of its targets; `None` for a document with no target.
    pub fn link_norm(&self, doc: Doc<'_>) -> Result<Option<f64>, WeightLimit> {
        self.norm(doc, Kind::Link)
    }

    /// `doc`'s lexical vector: each word's `tf × idf` divided by the norm, by word. Empty for a
    /// document with no word or whose raw weights are all 0.
    pub fn lexical(&self, doc: Doc<'_>) -> Result<Vec<(&str, f64)>, WeightLimit> {
        let Some(norm) = self.lexical_norm(doc)?.filter(|n| *n > 0.0) else {
            return Ok(vec![]);
        };
        let Some(counts) = self.counts(doc) else {
            return Ok(vec![]);
        };
        let c = self.lock();
        Ok(counts
            .iter()
            .map(|(w, n)| (w, tf(n) * c.idf[w] / norm))
            .collect())
    }

    /// `doc`'s link vector: each target's weight divided by the norm, by target. Empty for a
    /// document with no target or whose weights are all 0.
    pub fn link(&self, doc: Doc<'_>) -> Result<Vec<(&BlockId, f64)>, WeightLimit> {
        let Some(norm) = self.link_norm(doc)?.filter(|n| *n > 0.0) else {
            return Ok(vec![]);
        };
        let Some(targets) = self.targets(doc) else {
            return Ok(vec![]);
        };
        let c = self.lock();
        Ok(targets.iter().map(|t| (t, c.targets[t] / norm)).collect())
    }

    /// The norm of `doc`'s vector of `kind`, computed once. The charge of the norm and of every
    /// feature weight it needs that is not cached is taken first, before anything is computed
    /// or stored, so a refused read leaves the cache as it was.
    fn norm(&self, doc: Doc<'_>, kind: Kind) -> Result<Option<f64>, WeightLimit> {
        let key_len = match doc {
            Doc::Block(id) => id.as_str().len() + BLOCK_ENTRY_BYTES,
            Doc::Source => size_of::<f64>(),
        };
        match kind {
            Kind::Lexical => {
                let Some(counts) = self.counts(doc) else {
                    return Ok(None);
                };
                let (n, mut c) = (self.corpus_words(), self.lock());
                if let Some(v) = cached(&c, doc, kind) {
                    return Ok(Some(v));
                }
                let missing: usize = counts
                    .iter()
                    .filter(|(w, _)| !c.idf.contains_key(*w))
                    .map(|(w, _)| WORD_ENTRY_BYTES + w.len())
                    .sum();
                c.charge(key_len + missing, self.limit)?;
                let mut raw = Vec::with_capacity(counts.len());
                for (w, count) in counts.iter() {
                    let v = match c.idf.get(w) {
                        Some(v) => *v,
                        None => {
                            let v = idf(n, self.df(w));
                            c.idf.insert(w.to_string(), v);
                            v
                        }
                    };
                    raw.push(tf(count) * v);
                }
                let norm = length(raw.into_iter());
                store(&mut c, doc, kind, norm);
                Ok(Some(norm))
            }
            Kind::Link => {
                let Some(targets) = self.targets(doc) else {
                    return Ok(None);
                };
                let (l, mut c) = (self.corpus_links(), self.lock());
                if let Some(v) = cached(&c, doc, kind) {
                    return Ok(Some(v));
                }
                let missing: usize = targets
                    .iter()
                    .filter(|t| !c.targets.contains_key(*t))
                    .map(|t| BLOCK_ENTRY_BYTES + t.as_str().len())
                    .sum();
                c.charge(key_len + missing, self.limit)?;
                let mut weights = Vec::with_capacity(targets.len());
                for t in targets.iter() {
                    let v = match c.targets.get(t) {
                        Some(v) => *v,
                        None => {
                            let v = idf(l, self.indeg(t));
                            c.targets.insert(t.clone(), v);
                            v
                        }
                    };
                    weights.push(v);
                }
                let norm = length(weights.into_iter());
                store(&mut c, doc, kind, norm);
                Ok(Some(norm))
            }
        }
    }
}

/// The cached norm of `doc`'s vector of `kind`, if computed.
fn cached(c: &Cache, doc: Doc<'_>, kind: Kind) -> Option<f64> {
    match (doc, kind) {
        (Doc::Block(id), Kind::Lexical) => c.lexical.get(id).copied(),
        (Doc::Block(id), Kind::Link) => c.link.get(id).copied(),
        (Doc::Source, Kind::Lexical) => c.source_lexical,
        (Doc::Source, Kind::Link) => c.source_link,
    }
}

/// Stores the norm of `doc`'s vector of `kind`.
fn store(c: &mut Cache, doc: Doc<'_>, kind: Kind, norm: f64) {
    match (doc, kind) {
        (Doc::Block(id), Kind::Lexical) => {
            c.lexical.insert(id.clone(), norm);
        }
        (Doc::Block(id), Kind::Link) => {
            c.link.insert(id.clone(), norm);
        }
        (Doc::Source, Kind::Lexical) => c.source_lexical = Some(norm),
        (Doc::Source, Kind::Link) => c.source_link = Some(norm),
    }
}

// ---------------------------------------------------------------------------------------------
// Scoring (D-ZX5S6, D-RY7B5, D-0ZPAP): the one scorer that advice, review and search share.
// ---------------------------------------------------------------------------------------------

/// D-RY7B5: the most seed words one source finds candidates with.
pub const MAX_SEED_WORDS: usize = 64;

/// D-RY7B5: the most seed link targets one source finds candidates with.
pub const MAX_SEED_TARGETS: usize = 64;

/// The most shared words, and the most shared link targets, a reason lists.
pub const MAX_REASON_ITEMS: usize = 5;

/// D-ZX5S6: the share of the lexical dot product in a score when both blocks have links.
pub const LEXICAL_SHARE: f64 = 0.6;

/// D-ZX5S6: the share of the link dot product in a score when both blocks have links.
pub const LINK_SHARE: f64 = 0.4;

/// The most eligible-seed posting incidences one source may read, duplicates counted before the
/// union.
pub const SOURCE_POSTINGS: u64 = 200_000;

/// The most distinct candidate ids one source may have, itself included.
pub const SOURCE_CANDIDATES: u64 = 50_000;

/// The most sparse feature products one source may compute.
pub const SOURCE_PRODUCTS: u64 = 2_000_000;

/// The most charged candidate scratch payload one source may hold, in bytes: 64 MiB.
pub const SCRATCH_BYTES: u64 = 64 * 1024 * 1024;

/// What one candidate record is charged beside its id's bytes: its entry in the union and its
/// scored row.
pub const CANDIDATE_RECORD_BYTES: u64 =
    (size_of::<&BlockId>() + size_of::<Scored<'static>>()) as u64;

/// Which work guard stopped a source. Advice drops the source and counts it; review keeps the
/// complete sources; ranked search returns its vector-work refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// The source's seed postings passed [`SourceLimits::postings`].
    SourcePostings,
    /// The source's distinct candidates passed [`SourceLimits::candidates`].
    SourceCandidates,
    /// The source's products passed [`SourceLimits::products`].
    SourceProducts,
    /// The candidate scratch passed [`SourceLimits::scratch`].
    Scratch,
    /// The whole budget's postings ran out.
    SectionPostings,
    /// The whole budget's products ran out.
    SectionProducts,
    /// A lazy weight read did not fit ([`WeightLimit`]).
    Weights,
}

impl From<WeightLimit> for Guard {
    fn from(_: WeightLimit) -> Guard {
        Guard::Weights
    }
}

/// The per-source work limits of a [`VectorBudget`]. Every field is a fixed constant in
/// production ([`SourceLimits::FIXED`]); tests lower them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLimits {
    pub postings: u64,
    pub candidates: u64,
    pub products: u64,
    pub scratch: u64,
}

impl SourceLimits {
    /// The fixed limits: [`SOURCE_POSTINGS`], [`SOURCE_CANDIDATES`], [`SOURCE_PRODUCTS`] and
    /// [`SCRATCH_BYTES`].
    pub const FIXED: SourceLimits = SourceLimits {
        postings: SOURCE_POSTINGS,
        candidates: SOURCE_CANDIDATES,
        products: SOURCE_PRODUCTS,
        scratch: SCRATCH_BYTES,
    };
}

/// What a [`VectorBudget`] has charged, over the sources scored so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetUse {
    /// Posting incidences of selected seeds, duplicates counted.
    pub postings: u64,
    /// Distinct candidate ids, summed over the sources' unions (each source itself included).
    pub candidates: u64,
    /// Candidates scored in full (the ones the caller kept).
    pub pairs: u64,
    /// Sparse feature products: one per feature two scored blocks share.
    pub products: u64,
}

/// What one source has charged, to check against the [`SourceLimits`].
#[derive(Clone, Copy, Debug, Default)]
struct SourceUse {
    postings: u64,
    candidates: u64,
    products: u64,
    scratch: u64,
}

/// The work budget of one advice section, receipt, review phase or search request: a whole-budget
/// limit on posting incidences and on products, and the per-source limits. Every charge happens
/// before the work it pays for, and a charge that does not fit is refused and changes nothing. Work
/// a stopped source already charged stays charged, so the sources after it keep to what is left.
#[derive(Clone, Debug)]
pub struct VectorBudget {
    postings: u64,
    products: u64,
    source: SourceLimits,
    used: BudgetUse,
    current: SourceUse,
}

impl VectorBudget {
    /// A budget of `postings` posting incidences and `products` products, with the fixed
    /// per-source limits.
    pub const fn new(postings: u64, products: u64) -> VectorBudget {
        VectorBudget {
            postings,
            products,
            source: SourceLimits::FIXED,
            used: BudgetUse {
                postings: 0,
                candidates: 0,
                pairs: 0,
                products: 0,
            },
            current: SourceUse {
                postings: 0,
                candidates: 0,
                products: 0,
                scratch: 0,
            },
        }
    }

    /// A briefing section: 2,000,000 posting incidences and 8,000,000 products.
    pub const fn briefing() -> VectorBudget {
        VectorBudget::new(2_000_000, 8_000_000)
    }

    /// A write receipt: 100,000 posting incidences and 1,000,000 products.
    pub const fn write() -> VectorBudget {
        VectorBudget::new(100_000, 1_000_000)
    }

    /// One change's review vector phase: 200,000 posting incidences and 2,000,000 products.
    pub const fn review() -> VectorBudget {
        VectorBudget::new(200_000, 2_000_000)
    }

    /// One ranked search request: 2,000,000 posting incidences and 8,000,000 products.
    pub const fn search() -> VectorBudget {
        VectorBudget::new(2_000_000, 8_000_000)
    }

    /// Test hook: this budget with other per-source limits.
    #[doc(hidden)]
    pub const fn with_source_limits(mut self, limits: SourceLimits) -> VectorBudget {
        self.source = limits;
        self
    }

    /// The per-source limits.
    pub const fn source_limits(&self) -> SourceLimits {
        self.source
    }

    /// What the budget has charged so far.
    pub const fn used(&self) -> BudgetUse {
        self.used
    }

    /// Starts a source: its own counters begin at zero. The whole-budget counters carry on.
    pub fn begin_source(&mut self) {
        self.current = SourceUse::default();
    }

    /// Charges `n` posting incidences to the source and the whole budget.
    pub fn charge_postings(&mut self, n: u64) -> Result<(), Guard> {
        let source = self.current.postings.checked_add(n);
        if source.is_none_or(|v| v > self.source.postings) {
            return Err(Guard::SourcePostings);
        }
        let total = self.used.postings.checked_add(n);
        if total.is_none_or(|v| v > self.postings) {
            return Err(Guard::SectionPostings);
        }
        self.current.postings = source.unwrap_or(0);
        self.used.postings = total.unwrap_or(0);
        Ok(())
    }

    /// Charges `n` distinct candidate ids, with `bytes` of scratch payload, to the source.
    pub fn charge_candidates(&mut self, n: u64, bytes: u64) -> Result<(), Guard> {
        let count = self.current.candidates.checked_add(n);
        if count.is_none_or(|v| v > self.source.candidates) {
            return Err(Guard::SourceCandidates);
        }
        let scratch = self.current.scratch.checked_add(bytes);
        if scratch.is_none_or(|v| v > self.source.scratch) {
            return Err(Guard::Scratch);
        }
        self.current.candidates = count.unwrap_or(0);
        self.current.scratch = scratch.unwrap_or(0);
        self.used.candidates = self.used.candidates.saturating_add(n);
        Ok(())
    }

    /// Charges `n` products to the source and the whole budget.
    pub fn charge_products(&mut self, n: u64) -> Result<(), Guard> {
        let source = self.current.products.checked_add(n);
        if source.is_none_or(|v| v > self.source.products) {
            return Err(Guard::SourceProducts);
        }
        let total = self.used.products.checked_add(n);
        if total.is_none_or(|v| v > self.products) {
            return Err(Guard::SectionProducts);
        }
        self.current.products = source.unwrap_or(0);
        self.used.products = total.unwrap_or(0);
        Ok(())
    }

    /// Counts one candidate scored in full.
    fn count_pair(&mut self) {
        self.used.pairs = self.used.pairs.saturating_add(1);
    }
}

/// One document's two unit vectors: its words and its link targets, each by key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Operand<'w> {
    pub lexical: Vec<(&'w str, f64)>,
    pub link: Vec<(&'w BlockId, f64)>,
}

impl<'w> Operand<'w> {
    /// `doc`'s vectors under `w`, computing what is not cached yet.
    pub fn of(w: &'w VectorWeights, doc: Doc<'_>) -> Result<Operand<'w>, Guard> {
        Ok(Operand {
            lexical: w.lexical(doc)?,
            link: w.link(doc)?,
        })
    }
}

/// D-RY7B5: the seeds a source finds candidates with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seeds<'w> {
    /// Up to [`MAX_SEED_WORDS`] seed words, heaviest first.
    pub words: Vec<&'w str>,
    /// Up to [`MAX_SEED_TARGETS`] seed link targets, heaviest first.
    pub targets: Vec<&'w BlockId>,
}

impl Seeds<'_> {
    /// Whether the source has no seed, so it finds no candidate.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.targets.is_empty()
    }
}

/// The heaviest `cap` features of `vector` that `is_seed` accepts: by descending weight, then by
/// ascending key. The weights are the unit-vector weights the score uses.
fn heaviest<K: Ord + Copy>(
    vector: &[(K, f64)],
    cap: usize,
    mut is_seed: impl FnMut(K) -> bool,
) -> Vec<K> {
    let mut seeds: Vec<(K, f64)> = vector
        .iter()
        .copied()
        .filter(|(k, _)| is_seed(*k))
        .collect();
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    seeds.truncate(cap);
    seeds.into_iter().map(|(k, _)| k).collect()
}

/// D-RY7B5: `source`'s seeds. A word or target seeds when it is a corpus feature with
/// `df <= 100` or `10 × df <= N` (`indeg` and `L` for a target). The source keeps its 64 heaviest
/// seed words and its 64 heaviest seed targets, ties by word or target id. A feature that does not
/// seed never takes a slot, and still counts in every score.
pub fn select_seeds<'w>(w: &'w VectorWeights, source: &Operand<'w>) -> Seeds<'w> {
    Seeds {
        words: heaviest(&source.lexical, MAX_SEED_WORDS, |t| w.word_is_seed(t)),
        targets: heaviest(&source.link, MAX_SEED_TARGETS, |t| w.target_is_seed(t)),
    }
}

/// The candidates `seeds` find: the union of their postings, as distinct ids in id order. Each seed's
/// postings are charged before they are read, counting duplicates, and the distinct ids and their
/// scratch are charged before the caller scores any. A candidate can be the source itself; the
/// caller keeps or drops it. A feature below the seed cut finds nothing.
pub fn candidates<'w>(
    w: &'w VectorWeights,
    seeds: &Seeds<'w>,
    budget: &mut VectorBudget,
) -> Result<Vec<&'w BlockId>, Guard> {
    let lexicon = w.lexicon();
    let mut lists: Vec<&'w imbl::OrdSet<BlockId>> = Vec::new();
    let mut incidences: u64 = 0;
    for word in &seeds.words {
        if let Some(list) = lexicon.word_blocks_of(word) {
            incidences = incidences.saturating_add(list.len() as u64);
            lists.push(list);
        }
    }
    for target in &seeds.targets {
        if let Some(list) = lexicon.pointing_at(target) {
            incidences = incidences.saturating_add(list.len() as u64);
            lists.push(list);
        }
    }
    budget.charge_postings(incidences)?;
    let mut ids: Vec<&'w BlockId> = Vec::with_capacity(incidences as usize);
    for list in lists {
        ids.extend(list.iter());
    }
    ids.sort_unstable();
    ids.dedup();
    let bytes: u64 = ids
        .iter()
        .map(|id| CANDIDATE_RECORD_BYTES + id.as_str().len() as u64)
        .sum();
    budget.charge_candidates(ids.len() as u64, bytes)?;
    Ok(ids)
}

/// One scored candidate: its exact score. Round only when the score is shown ([`round3`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scored<'w> {
    pub block: &'w BlockId,
    pub score: f64,
}

/// The sparse dot product of two vectors sorted by key, summed in ascending key order. One
/// product is charged before each is computed.
fn dot<K: Ord>(a: &[(K, f64)], b: &[(K, f64)], budget: &mut VectorBudget) -> Result<f64, Guard> {
    let (mut i, mut j, mut sum) = (0, 0, 0.0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                budget.charge_products(1)?;
                sum += a[i].1 * b[j].1;
                i += 1;
                j += 1;
            }
        }
    }
    Ok(sum)
}

/// A score never passes 1: two unit vectors that are the same can add to 1 plus a rounding
/// error of a few ulp, and that is the only value this clamps.
fn clamp_unit(x: f64) -> f64 {
    if x > 1.0 { 1.0 } else { x }
}

/// D-ZX5S6: the score of two documents' vectors, `0.6 × lexical_dot + 0.4 × link_dot` when both
/// have link targets, else `lexical_dot`. Charges one product for each feature the two share.
pub fn score_operands(
    a: &Operand<'_>,
    b: &Operand<'_>,
    budget: &mut VectorBudget,
) -> Result<f64, Guard> {
    let lexical = dot(&a.lexical, &b.lexical, budget)?;
    let score = if !a.link.is_empty() && !b.link.is_empty() {
        let link = dot(&a.link, &b.link, budget)?;
        LEXICAL_SHARE * lexical + LINK_SHARE * link
    } else {
        lexical
    };
    Ok(clamp_unit(score))
}

/// The exact score of documents `a` and `b` (see [`score_operands`]), with every feature, whether
/// or not it seeds. Charges its products to `budget`.
pub fn score_pair(
    w: &VectorWeights,
    a: Doc<'_>,
    b: Doc<'_>,
    budget: &mut VectorBudget,
) -> Result<f64, Guard> {
    let (a, b) = (Operand::of(w, a)?, Operand::of(w, b)?);
    score_operands(&a, &b, budget)
}

/// Scores `source` against every candidate its seeds find that `keep` accepts. It starts a source
/// in `budget`, reads the source's vectors, selects its seeds and unions their postings, then
/// scores each kept candidate once with all its features, in id order. A source with no seed gives
/// an empty, complete result and charges nothing. A guard gives `Err` and no partial result.
/// `keep` runs before any product is charged, so a dropped candidate (the source itself
/// included) costs none.
pub fn score_source<'w>(
    w: &'w VectorWeights,
    source: Doc<'_>,
    mut keep: impl FnMut(&'w BlockId) -> bool,
    budget: &mut VectorBudget,
) -> Result<Vec<Scored<'w>>, Guard> {
    budget.begin_source();
    let src = Operand::of(w, source)?;
    let seeds = select_seeds(w, &src);
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let ids = candidates(w, &seeds, budget)?;
    let mut scored = Vec::new();
    for id in ids {
        if !keep(id) {
            continue;
        }
        let candidate = Operand::of(w, Doc::Block(id))?;
        budget.count_pair();
        let score = score_operands(&src, &candidate, budget)?;
        scored.push(Scored { block: id, score });
    }
    Ok(scored)
}

/// The score as shown: rounded to three decimals. Never sort or filter on it.
pub fn round3(score: f64) -> f64 {
    (score * 1000.0).round() / 1000.0
}

/// Ranks `rows`: drops those below `floor` (exact score, at least), sorts by descending exact
/// score, then by `tie` ascending, and keeps the first `cap`. `tie` runs only for rows whose exact
/// scores are equal, and not past the group that holds the cut. It must give distinct keys to
/// distinct rows. The ranking reads no formatting: nothing here rounds.
pub fn rank_rows<R, K: Ord>(
    mut rows: Vec<R>,
    score: impl Fn(&R) -> f64,
    floor: f64,
    cap: usize,
    mut tie: impl FnMut(&R) -> K,
) -> Vec<R> {
    rows.retain(|r| score(r) >= floor);
    rows.sort_by(|a, b| score(b).total_cmp(&score(a)));
    let mut out: Vec<R> = Vec::new();
    let mut rest = rows.into_iter().peekable();
    while out.len() < cap {
        let Some(first) = rest.next() else { break };
        let at = score(&first);
        let mut group = vec![first];
        while let Some(next) = rest.next_if(|n| score(n).total_cmp(&at).is_eq()) {
            group.push(next);
        }
        if group.len() > 1 {
            let mut keyed: Vec<(K, R)> = group.into_iter().map(|r| (tie(&r), r)).collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            group = keyed.into_iter().map(|(_, r)| r).collect();
        }
        out.extend(group);
    }
    out.truncate(cap);
    out
}

/// Why two documents match: their shared words and shared link targets with the largest weight
/// products.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reasons {
    /// Up to [`MAX_REASON_ITEMS`] shared words.
    pub words: Vec<String>,
    /// Up to [`MAX_REASON_ITEMS`] shared link targets.
    pub links: Vec<BlockId>,
}

/// The `MAX_REASON_ITEMS` shared keys of two sorted vectors with the largest weight products:
/// by descending product, then ascending key.
fn shared<K: Ord + Copy>(a: &[(K, f64)], b: &[(K, f64)]) -> Vec<K> {
    let (mut i, mut j) = (0, 0);
    let mut both: Vec<(K, f64)> = Vec::new();
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both.push((a[i].0, a[i].1 * b[j].1));
                i += 1;
                j += 1;
            }
        }
    }
    both.sort_by(|x, y| y.1.total_cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    both.truncate(MAX_REASON_ITEMS);
    both.into_iter().map(|(k, _)| k).collect()
}

/// The reasons of documents `a` and `b`: at most five shared words and five shared targets, each
/// list by descending weight product, then by item. Computed for the rows a caller keeps, so the
/// products are not charged.
pub fn reasons(w: &VectorWeights, a: Doc<'_>, b: Doc<'_>) -> Result<Reasons, Guard> {
    let (a, b) = (Operand::of(w, a)?, Operand::of(w, b)?);
    Ok(Reasons {
        words: shared(&a.lexical, &b.lexical)
            .into_iter()
            .map(str::to_string)
            .collect(),
        links: shared(&a.link, &b.link).into_iter().cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Materialized, Rdy, Session, WriteRequest, WriteResult};
    use rdy_model::Role;
    use rdy_model::{FieldValue, Link};
    use rdy_schema::{Pack, PackSet};
    use serde_json::{Value, json};
    use std::collections::BTreeSet;

    fn pack() -> Pack {
        PackSet::with_builtin().resolve("rdy-standard").unwrap()
    }

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    fn s(t: &str) -> FieldValue {
        FieldValue::Str(t.to_string())
    }

    /// A block of type `ty` with `body`, `fields` and `links` (link name, target), under
    /// `parent`.
    fn blk(
        i: &str,
        ty: &str,
        parent: Option<&str>,
        body: &str,
        fields: Vec<(&str, FieldValue)>,
        links: Vec<(&str, &str)>,
    ) -> Block {
        let mut by_name: BTreeMap<String, Vec<Link>> = BTreeMap::new();
        for (name, to) in links {
            by_name.entry(name.to_string()).or_default().push(Link {
                target: id(to),
                part: None,
            });
        }
        Block {
            id: id(i),
            block_type: ty.to_string(),
            parent: parent.map(id),
            order: "a1".to_string(),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            body: body.to_string(),
            links: by_name,
            pins: Default::default(),
            foreign: Default::default(),
            read_identity: None,
        }
    }

    fn lex(blocks: &[Block]) -> Arc<Lexicon> {
        Arc::new(Lexicon::build(&pack(), blocks.iter()))
    }

    fn counts_of(l: &Lexicon, i: &str) -> Vec<(String, u64)> {
        l.word_counts(&id(i))
            .map(|c| c.iter().map(|(w, n)| (w.to_string(), n)).collect())
            .unwrap_or_default()
    }

    fn pairs(v: &[(&str, u64)]) -> Vec<(String, u64)> {
        v.iter().map(|(w, n)| (w.to_string(), *n)).collect()
    }

    #[test]
    fn vxs1_counts_use_lexicon_sources_without_folding() {
        let blocks = vec![
            // a reference is blanked, the enum field `priority` and the metadata field `status`
            // are never read; `index.redb` and `rdy-core` are one word each; no plural or
            // Unicode fold beyond lower case
            blk(
                "r-00001",
                "requirement",
                None,
                "The cache caches Cache. See {{c-00002}} and `code_x` in index.redb, rdy-core: \
                 Straße STRASSE naïve.",
                vec![("priority", s("must")), ("status", s("draft"))],
                vec![],
            ),
            // a list field counts each item
            blk(
                "f-00001",
                "flow",
                None,
                "",
                vec![
                    ("name", s("Cache flow")),
                    (
                        "steps",
                        FieldValue::List(vec![s("Fill the cache"), s("Drop the cache")]),
                    ),
                ],
                vec![],
            ),
            blk("c-00002", "claim", None, "Must hold.", vec![], vec![]),
            blk(
                "m-00001",
                "component",
                None,
                "",
                vec![
                    ("name", s("Cache Store")),
                    ("aliases", FieldValue::List(vec![s("stores")])),
                ],
                vec![],
            ),
            // a code body is not read, and `lang` is a metadata field
            blk(
                "cd-00001",
                "code",
                None,
                "let cache = 1;",
                vec![("lang", s("rust"))],
                vec![],
            ),
            // a section whose title holds no word has no counts
            blk(
                "s-00001",
                "section",
                None,
                "",
                vec![("title", s("— · —"))],
                vec![],
            ),
        ];
        let l = lex(&blocks);
        assert_eq!(
            counts_of(&l, "r-00001"),
            pairs(&[
                ("and", 1),
                ("cache", 2),
                ("caches", 1),
                ("code_x", 1),
                ("in", 1),
                ("index.redb", 1),
                ("naïve", 1),
                ("rdy-core", 1),
                ("see", 1),
                ("strasse", 1),
                ("straße", 1),
                ("the", 1),
            ])
        );
        assert_eq!(
            counts_of(&l, "f-00001"),
            pairs(&[
                ("cache", 3),
                ("drop", 1),
                ("fill", 1),
                ("flow", 1),
                ("the", 2)
            ])
        );
        assert_eq!(counts_of(&l, "c-00002"), pairs(&[("hold", 1), ("must", 1)]));
        assert_eq!(
            counts_of(&l, "m-00001"),
            pairs(&[("cache", 1), ("store", 1), ("stores", 1)])
        );
        assert!(l.word_counts(&id("cd-00001")).is_none());
        assert!(l.word_counts(&id("s-00001")).is_none());
        let wc = l.word_counts(&id("r-00001")).unwrap();
        assert_eq!((wc.get("cache"), wc.get("must"), wc.len()), (2, 0, 12));
        // N counts the blocks with a word; df counts blocks, not occurrences
        assert_eq!(l.word_blocks(), 4);
        assert_eq!(
            ["cache", "the", "must", "c-00002", "let"].map(|w| l.word_df(w)),
            [3, 2, 1, 0, 0]
        );
        // the lexical membership, literal lookup and name matching read as before
        assert_eq!(
            l.blocks_with_words("cache"),
            vec![id("f-00001"), id("m-00001"), id("r-00001")]
        );
        assert_eq!(l.literal_blocks("code_x"), vec![id("r-00001")]);
        assert_eq!(
            l.resolve("cache stores"),
            vec![(id("m-00001"), crate::lexicon::NameKind::Name)]
        );
        assert_eq!(l.resolve("stores")[0].0, id("m-00001"));
    }

    #[test]
    fn vxs1_link_targets_are_unique_and_exclude_contains() {
        let blocks = vec![
            blk(
                "s-00001",
                "section",
                None,
                "",
                vec![("title", s("Terms"))],
                vec![],
            ),
            // two references and a describes link to one target, two describes links to
            // another: each target once; the parent is no target
            blk(
                "n-00001",
                "note",
                Some("s-00001"),
                "See {{c-00002}}, again {{c-00002}}, and {{en-00001}}.",
                vec![],
                vec![
                    ("describes", "en-00001"),
                    ("describes", "en-00001"),
                    ("implements", "r-00001"),
                    ("describes", "c-00002"),
                ],
            ),
            blk("c-00002", "claim", None, "Plain.", vec![], vec![]),
            blk(
                "p-00001",
                "prose",
                None,
                "Also {{c-00002}}.",
                vec![],
                vec![],
            ),
        ];
        let l = lex(&blocks);
        let targets = |i: &str| -> Vec<BlockId> {
            l.link_targets(&id(i))
                .map(|t| t.iter().cloned().collect())
                .unwrap_or_default()
        };
        assert_eq!(
            targets("n-00001"),
            vec![id("c-00002"), id("en-00001"), id("r-00001")]
        );
        assert_eq!(targets("p-00001"), vec![id("c-00002")]);
        // a section's children are no targets of it, and a block with none is not in L
        assert!(l.link_targets(&id("s-00001")).is_none());
        assert!(l.link_targets(&id("c-00002")).is_none());
        assert_eq!(l.link_blocks(), 2);
        assert_eq!(
            ["c-00002", "en-00001", "r-00001", "s-00001", "n-00001"].map(|t| l.indeg(&id(t))),
            [2, 1, 1, 0, 0]
        );
        let t = l.link_targets(&id("n-00001")).unwrap();
        assert!(t.contains(&id("r-00001")) && !t.contains(&id("s-00001")));
        assert_eq!(t.len(), 3);
    }

    fn writer() -> Session {
        Session::human("human:founder".into(), Role::Writer).unwrap()
    }

    fn approver() -> Session {
        Session::human("human:founder".into(), Role::Approver).unwrap()
    }

    fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
        r.write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "w".into(),
            },
        )
        .unwrap()
    }

    fn land(r: &mut Rdy, view: &str) {
        let p = r
            .propose(&writer(), view, "t", Some("test summary"))
            .unwrap();
        r.land(&approver(), &p.id).unwrap();
    }

    /// A doc with a section and `n` notes that share some words and reference each other.
    fn doc_ops(tag: &str, n: usize) -> Value {
        let mut ops = vec![
            json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": format!("Guide {tag}"), "doc_type": "page"}}),
            json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}),
            json!({"op": "create", "type": "section", "id": "$t", "parent": "$d", "fields": {"title": "More"}}),
        ];
        for i in 0..n {
            let body = if i == 0 {
                format!("The {tag} gateway feeds the ledger ledger.")
            } else {
                format!("The {tag} part {i} feeds the gateway, see {{{{$n0}}}}.")
            };
            ops.push(json!({"op": "create", "type": "note", "id": format!("$n{i}"), "parent": "$s", "body": body}));
        }
        json!(ops)
    }

    /// Every weight of `w` over `m`'s blocks, as exact bits, in id order: N and L, then each
    /// block's lexical and link vectors.
    fn weight_bits(w: &VectorWeights, m: &Materialized) -> Vec<String> {
        let mut out = vec![format!("N={} L={}", w.corpus_words(), w.corpus_links())];
        for b in m.iter() {
            let d = Doc::Block(&b.id);
            for (word, v) in w.lexical(d).unwrap() {
                out.push(format!("{} {word} {:016x}", b.id, v.to_bits()));
            }
            for (t, v) in w.link(d).unwrap() {
                out.push(format!("{} ->{t} {:016x}", b.id, v.to_bits()));
            }
        }
        out
    }

    /// `scope`'s lexicon and weights as `st` carries them, against a rebuild from the store.
    fn assert_carried(st: &crate::ReadState, scope: &Scope, what: &str) {
        let m = st.materialized(scope).unwrap();
        let carried = st.lexicon(scope).unwrap();
        let (_, _, full) = crate::testkit::rebuilt_scope_for_tests(st, scope).unwrap();
        assert!(
            *carried == full,
            "{what}: the carried counts equal a rebuild"
        );
        let w = st.vector_weights(WeightScope::Read(scope)).unwrap();
        let rebuilt = VectorWeights::of(Arc::new(full));
        assert_eq!(
            weight_bits(&w, &m),
            weight_bits(&rebuilt, &m),
            "{what}: the weights equal a rebuild's, bit for bit"
        );
    }

    /// Checks canon and every view of `r`'s current state.
    fn assert_all_carried(r: &Rdy, step: &str) {
        let st = r.read();
        assert_carried(&st, &Scope::Canon, &format!("{step}: canon"));
        for v in st.view_names() {
            assert_carried(&st, &Scope::View(v.clone()), &format!("{step}: {v}"));
        }
    }

    #[test]
    fn vxs1_carried_counts_equal_rebuild_after_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let first = write(&mut r, "work", doc_ops("alpha", 4));
        land(&mut r, "work");
        assert_all_carried(&r, "first landing");
        let n1 = first.created["$n1"].clone();
        let n2 = first.created["$n2"].clone();
        let n3 = first.created["$n3"].clone();
        let more = first.created["$t"].clone();
        // create, update, delete and move, each in view `a`
        write(&mut r, "a", doc_ops("beta", 3));
        assert_all_carried(&r, "create");
        write(
            &mut r,
            "a",
            json!([{"op": "update", "id": n1, "body": "Ledger ledger ledger, now rewritten."}]),
        );
        assert_all_carried(&r, "update");
        write(&mut r, "a", json!([{"op": "delete", "id": n2}]));
        assert_all_carried(&r, "delete");
        write(
            &mut r,
            "a",
            json!([{"op": "move", "id": n3, "parent": more}]),
        );
        assert_all_carried(&r, "move");
        // view `b` has its own change when `a` lands: it reads on the new canon (rebase)
        let b1 = write(&mut r, "b", doc_ops("gamma", 2));
        let b2 = write(
            &mut r,
            "b",
            json!([{"op": "update", "id": b1.created["$n1"], "body": "Gamma gamma, the second take."}]),
        );
        assert_all_carried(&r, "before the landing");
        land(&mut r, "a");
        assert_all_carried(&r, "landing and rebase");
        // include and exclude in view `c`
        r.view_include(&writer(), "c", &[b1.change.clone(), b2.change.clone()])
            .unwrap();
        assert_all_carried(&r, "include");
        r.view_exclude(&writer(), "c", std::slice::from_ref(&b2.change), false)
            .unwrap();
        assert_all_carried(&r, "exclude");
        // a corpus change that moves every idf: a new block of new words changes N alone
        let st = r.read();
        let old = st.vector_weights(WeightScope::Read(&Scope::Canon)).unwrap();
        let words: Vec<String> = {
            let m = st.materialized(&Scope::Canon).unwrap();
            let mut ws: BTreeSet<String> = BTreeSet::new();
            for b in m.iter() {
                if let Some(c) = old.counts(Doc::Block(&b.id)) {
                    ws.extend(c.iter().map(|(w, _)| w.to_string()));
                }
            }
            ws.into_iter().collect()
        };
        let parent = first.created["$s"].clone();
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "note", "id": "$x", "parent": parent, "body": "Zebra quokka."}]),
        );
        land(&mut r, "work");
        assert_all_carried(&r, "every idf moves");
        let st = r.read();
        let new = st.vector_weights(WeightScope::Read(&Scope::Canon)).unwrap();
        assert_eq!(new.corpus_words(), old.corpus_words() + 1);
        for w in &words {
            assert_eq!(new.df(w), old.df(w), "{w}: df holds");
            let (a, b) = (old.idf(w).unwrap().unwrap(), new.idf(w).unwrap().unwrap());
            assert_ne!(a.to_bits(), b.to_bits(), "{w}: idf moves with N");
        }
    }

    #[test]
    fn vxs1_weight_scope_counts_empty_and_overlay_blocks_exactly() {
        // blocks with no word or no target are not in N or L
        let blocks = vec![
            blk(
                "s-00001",
                "section",
                None,
                "",
                vec![("title", s("— —"))],
                vec![],
            ),
            blk("c-00001", "claim", None, "Alpha beta.", vec![], vec![]),
            blk(
                "c-00002",
                "claim",
                None,
                "Beta gamma {{c-00001}}.",
                vec![],
                vec![],
            ),
            blk(
                "c-00003",
                "claim",
                None,
                "…",
                vec![],
                vec![("implements", "c-00001")],
            ),
        ];
        let w = VectorWeights::of(lex(&blocks));
        assert_eq!((w.corpus_words(), w.corpus_links()), (2, 2));
        assert_eq!((w.df("beta"), w.df("alpha"), w.df("delta")), (2, 1, 0));
        assert_eq!(w.indeg(&id("c-00001")), 2);
        let s1 = id("s-00001");
        assert!(w.counts(Doc::Block(&s1)).is_none());
        assert!(w.lexical(Doc::Block(&s1)).unwrap().is_empty());
        assert!(w.link(Doc::Block(&s1)).unwrap().is_empty());
        assert!(w.counts(Doc::Source).is_none() && w.source().is_none());
        let c3 = id("c-00003");
        assert!(w.lexical(Doc::Block(&c3)).unwrap().is_empty());
        assert_eq!(w.link(Doc::Block(&c3)).unwrap().len(), 1);
        assert_eq!(w.idf("delta").unwrap(), None);

        // the overlay: a source whose id canon holds counts as one more corpus block
        let source = blk(
            "c-00001",
            "claim",
            None,
            "Beta delta delta {{c-00002}}.",
            vec![],
            vec![],
        );
        let o = VectorWeights::review(lex(&blocks), &source);
        assert_eq!(o.source(), Some(&id("c-00001")));
        assert_eq!((o.corpus_words(), o.corpus_links()), (3, 3));
        assert_eq!(
            (o.df("beta"), o.df("alpha"), o.df("delta"), o.df("gamma")),
            (3, 1, 1, 1)
        );
        assert_eq!((o.indeg(&id("c-00002")), o.indeg(&id("c-00001"))), (1, 2));
        assert_eq!(o.counts(Doc::Source).unwrap().get("delta"), 2);
        // canon's own c-00001 keeps its canon words
        assert_eq!(
            o.counts(Doc::Block(&id("c-00001"))).unwrap().get("delta"),
            0
        );
        let idf_beta = idf(3, 3);
        assert_eq!(o.idf("beta").unwrap(), Some(idf_beta));
        let raw = [tf(1) * idf_beta, tf(2) * idf(3, 1)];
        let norm = length(raw.into_iter());
        assert_eq!(o.lexical_norm(Doc::Source).unwrap(), Some(norm));
        let v = o.lexical(Doc::Source).unwrap();
        assert_eq!(v, vec![("beta", raw[0] / norm), ("delta", raw[1] / norm)]);
        // a source with no word or target adds no corpus block
        let empty = blk("c-00009", "claim", None, "…", vec![], vec![]);
        let e = VectorWeights::review(lex(&blocks), &empty);
        assert_eq!((e.corpus_words(), e.corpus_links()), (2, 2));
        assert!(e.lexical(Doc::Source).unwrap().is_empty());

        // through a read state: review reads canon and the source, never another view
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        write(&mut r, "work", doc_ops("alpha", 3));
        land(&mut r, "work");
        write(&mut r, "side", doc_ops("delta", 3));
        let st = r.read();
        let canon = st.vector_weights(WeightScope::Read(&Scope::Canon)).unwrap();
        let side = st
            .vector_weights(WeightScope::Read(&Scope::View("side".into())))
            .unwrap();
        assert!(side.corpus_words() > canon.corpus_words());
        let block = st
            .materialized(&Scope::View("side".into()))
            .unwrap()
            .iter()
            .find(|b| b.body.contains("delta gateway"))
            .cloned()
            .unwrap();
        let review = st.vector_weights(WeightScope::Review(&block)).unwrap();
        assert_eq!(review.corpus_words(), canon.corpus_words() + 1);
        assert_eq!(review.df("delta"), 1);
        assert_eq!(review.df("gateway"), canon.df("gateway") + 1);
        // review's weights are made per call, never cached under the block's id
        let again = st.vector_weights(WeightScope::Review(&block)).unwrap();
        assert!(!Arc::ptr_eq(&review, &again));
    }

    /// The numerical golden of the weights: its constants, log cases, blocks and expected bits.
    fn golden() -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rdy-qa/fixtures/vectors/m5a-v1.json");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        serde_json::from_str(&text).unwrap()
    }

    fn bits(v: &Value) -> f64 {
        f64::from_bits(u64::from_str_radix(v.as_str().unwrap(), 16).unwrap())
    }

    fn hex(v: f64) -> String {
        format!("{:016x}", v.to_bits())
    }

    /// The fixture's block: id, type, parent, body, string or list fields, and links.
    fn fixture_block(v: &Value) -> Block {
        let field = |x: &Value| match x {
            Value::String(t) => s(t),
            Value::Array(items) => {
                FieldValue::List(items.iter().map(|i| s(i.as_str().unwrap())).collect())
            }
            other => panic!("a fixture field is a string or a list: {other}"),
        };
        let fields: Vec<(&str, FieldValue)> = v["fields"]
            .as_object()
            .map(|m| m.iter().map(|(k, x)| (k.as_str(), field(x))).collect())
            .unwrap_or_default();
        let links: Vec<(&str, &str)> = v["links"]
            .as_object()
            .map(|m| {
                m.iter()
                    .flat_map(|(k, ts)| {
                        ts.as_array()
                            .unwrap()
                            .iter()
                            .map(move |t| (k.as_str(), t.as_str().unwrap()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        blk(
            v["id"].as_str().unwrap(),
            v["type"].as_str().unwrap(),
            v["parent"].as_str(),
            v["body"].as_str().unwrap_or(""),
            fields,
            links,
        )
    }

    #[test]
    fn vxs1_log_weights_and_norms_match_golden_bits() {
        let g = golden();
        // the constants
        let c = &g["constants"];
        assert_eq!(c["ln2_bits"], json!(format!("{LN2_BITS:016x}")));
        assert_eq!(c["log2_terms"], json!(LOG2_TERMS));
        assert_eq!(c["seed_floor"], json!(SEED_FLOOR));
        assert_eq!(c["seed_share"], json!(SEED_SHARE));
        assert_eq!(c["weight_cache_bytes"], json!(WEIGHT_CACHE_BYTES));
        assert_eq!(f64::from_bits(LN2_BITS), std::f64::consts::LN_2);
        // the log cases: powers of two, both range boundaries, subnormals, ratios
        let cases = g["log2"].as_array().unwrap();
        assert!(cases.len() >= 20);
        for case in cases {
            let x = bits(&case["x"]);
            assert_eq!(
                hex(log2_fixed(x)),
                case["log2"].as_str().unwrap(),
                "log2_fixed({x:e})"
            );
        }
        // every power of two, subnormals included, gives its exponent exactly
        for e in -1074i32..=1023 {
            let x = if e >= -1022 {
                f64::from_bits(((e + 1023) as u64) << 52)
            } else {
                f64::from_bits(1u64 << (e + 1074))
            };
            assert_eq!(log2_fixed(x), f64::from(e), "2^{e}");
        }
        assert!(log2_fixed(0.0).is_nan() && log2_fixed(-1.0).is_nan());
        assert!(log2_fixed(f64::INFINITY).is_nan() && log2_fixed(f64::NAN).is_nan());
        // the store: at most 64 blocks
        let blocks: Vec<Block> = g["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(fixture_block)
            .collect();
        assert!(blocks.len() <= 64);
        let w = VectorWeights::of(lex(&blocks));
        let x = &g["expected"];
        assert_eq!(x["corpus_words"], json!(w.corpus_words()));
        assert_eq!(x["corpus_links"], json!(w.corpus_links()));
        for (word, want) in x["idf"].as_object().unwrap() {
            assert_eq!(want["df"], json!(w.df(word)), "df {word}");
            assert_eq!(want["seed"], json!(w.word_is_seed(word)), "seed {word}");
            assert_eq!(
                hex(w.idf(word).unwrap().unwrap()),
                want["bits"].as_str().unwrap(),
                "idf {word}"
            );
        }
        for (t, want) in x["targets"].as_object().unwrap() {
            let t = id(t);
            assert_eq!(want["indeg"], json!(w.indeg(&t)), "indeg {t}");
            assert_eq!(want["seed"], json!(w.target_is_seed(&t)), "seed {t}");
            assert_eq!(
                hex(w.target_weight(&t).unwrap().unwrap()),
                want["bits"].as_str().unwrap(),
                "weight {t}"
            );
        }
        let per_block = x["blocks"].as_object().unwrap();
        assert_eq!(per_block.len(), blocks.len());
        for b in &blocks {
            let want = &per_block[b.id.as_str()];
            let d = Doc::Block(&b.id);
            let norm = |n: Option<f64>| n.map_or(Value::Null, |n| json!(hex(n)));
            assert_eq!(
                norm(w.lexical_norm(d).unwrap()),
                want["lexical_norm"],
                "{}",
                b.id
            );
            assert_eq!(norm(w.link_norm(d).unwrap()), want["link_norm"], "{}", b.id);
            let lexical: Vec<Value> = w
                .lexical(d)
                .unwrap()
                .into_iter()
                .map(|(k, v)| json!([k, hex(v)]))
                .collect();
            assert_eq!(json!(lexical), want["lexical"], "{}", b.id);
            let link: Vec<Value> = w
                .link(d)
                .unwrap()
                .into_iter()
                .map(|(k, v)| json!([k.as_str(), hex(v)]))
                .collect();
            assert_eq!(json!(link), want["link"], "{}", b.id);
            let counts: Vec<Value> = w
                .counts(d)
                .map(|c| c.iter().map(|(k, n)| json!([k, n])).collect())
                .unwrap_or_default();
            assert_eq!(json!(counts), want["counts"], "{}", b.id);
        }
        // every corpus word and target is in the golden
        let words: BTreeSet<&str> = blocks
            .iter()
            .filter_map(|b| w.counts(Doc::Block(&b.id)))
            .flat_map(|c| c.iter().map(|(k, _)| k))
            .collect();
        assert_eq!(words.len(), x["idf"].as_object().unwrap().len());
        let unicode = words.iter().filter(|k| !k.is_ascii()).count();
        assert!(unicode >= 3, "the golden holds Unicode words");
    }

    #[test]
    fn vxs1_snapshot_cache_is_lazy_and_old_readers_stay_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let first = write(&mut r, "work", doc_ops("alpha", 3));
        land(&mut r, "work");
        let note = first.created["$n0"].clone();
        let nid = id(&note);
        let st1 = r.read();
        let w1 = st1
            .vector_weights(WeightScope::Read(&Scope::Canon))
            .unwrap();
        // nothing is computed until a read asks
        assert_eq!(w1.stats(), WeightStats::default());
        let v1 = w1
            .lexical(Doc::Block(&nid))
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_bits()))
            .collect::<Vec<_>>();
        let stats1 = w1.stats();
        assert_eq!(stats1.lexical_norms, 1);
        // once per snapshot: the state hands out the same weights
        let again = st1
            .vector_weights(WeightScope::Read(&Scope::Canon))
            .unwrap();
        assert!(Arc::ptr_eq(&w1, &again));
        // a corpus change: the new state starts with no lazy value; the write computed none
        let parent = first.created["$s"].clone();
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "note", "id": "$x", "parent": parent, "body": "Alpha zebra."}]),
        );
        land(&mut r, "work");
        let st2 = r.read();
        assert!(
            st2.built(&Scope::Canon).unwrap().weights.get().is_none(),
            "the publish computed no weights"
        );
        assert!(st2.lexicon_built(&Scope::Canon), "the lexicon was carried");
        let w2 = st2
            .vector_weights(WeightScope::Read(&Scope::Canon))
            .unwrap();
        assert!(!Arc::ptr_eq(&w1, &w2));
        assert_eq!(w2.stats(), WeightStats::default());
        let v2 = w2
            .lexical(Doc::Block(&nid))
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_bits()))
            .collect::<Vec<_>>();
        assert_ne!(v1, v2, "N moved, so the weights moved");
        // the held reader keeps its own values, and the new reader never touched them
        let held = w1
            .lexical(Doc::Block(&nid))
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_bits()))
            .collect::<Vec<_>>();
        assert_eq!(held, v1);
        assert_eq!(w1.stats(), stats1);
        // an unrelated reader (a view with its own change) has its own weights
        write(&mut r, "side", doc_ops("beta", 2));
        let st3 = r.read();
        let canon3 = st3
            .vector_weights(WeightScope::Read(&Scope::Canon))
            .unwrap();
        let side = st3
            .vector_weights(WeightScope::Read(&Scope::View("side".into())))
            .unwrap();
        assert!(!Arc::ptr_eq(&canon3, &side));
        let before = canon3.stats();
        side.lexical_norm(Doc::Block(&nid)).unwrap();
        assert_eq!(canon3.stats(), before);
        // a floating view reads canon's weights
        r.view_create(&writer(), "float", None, false).unwrap();
        let st4 = r.read();
        let c4 = st4
            .vector_weights(WeightScope::Read(&Scope::Canon))
            .unwrap();
        let f4 = st4
            .vector_weights(WeightScope::Read(&Scope::View("float".into())))
            .unwrap();
        assert!(Arc::ptr_eq(&c4, &f4));
    }

    /// `n` notes whose words are `u<i>` alone, and `common` in the first `with` of them.
    fn common_blocks(n: usize, with: usize, target: Option<usize>) -> Vec<Block> {
        (0..n)
            .map(|i| {
                let mut body = format!("u{i}");
                if i < with {
                    body.push_str(" common");
                }
                if target.is_some_and(|t| i < t) {
                    body.push_str(" {{c-00000}}");
                }
                blk(&format!("n-{i:05}"), "note", None, &body, vec![], vec![])
            })
            .collect()
    }

    #[test]
    fn vxs1_seed_frequency_boundaries_keep_nonseed_weights() {
        // the predicate, in integers
        assert!(is_seed(100, 0) && is_seed(100, 1_000_000));
        assert!(!is_seed(101, 1009) && is_seed(101, 1010));
        assert!(is_seed(1, 1) && !is_seed(200, 1999) && is_seed(200, 2000));
        assert!(!is_seed(u64::MAX, u64::MAX) && !is_seed(u64::MAX / 9, u64::MAX));
        // 101 of 1009 blocks hold `common`: not a seed, still weighed and normed
        let blocks = common_blocks(1009, 101, None);
        let w = VectorWeights::of(lex(&blocks));
        assert_eq!((w.corpus_words(), w.df("common")), (1009, 101));
        assert!(!w.word_is_seed("common") && w.word_is_seed("u0"));
        let idf_common = w.idf("common").unwrap().unwrap();
        assert_eq!(idf_common, idf(1009, 101));
        assert!(idf_common > 0.0);
        let n0 = id("n-00000");
        let raw = [tf(1) * idf_common, tf(1) * idf(1009, 1)];
        let norm = length(raw.into_iter());
        assert_eq!(w.lexical_norm(Doc::Block(&n0)).unwrap(), Some(norm));
        assert_eq!(
            w.lexical(Doc::Block(&n0)).unwrap(),
            vec![("common", raw[0] / norm), ("u0", raw[1] / norm)]
        );
        // one block more: 10 × 101 <= 1010 seeds it
        let w = VectorWeights::of(lex(&common_blocks(1010, 101, None)));
        assert!(w.word_is_seed("common"));
        // the floor: 100 of 1010 blocks seed, whatever N
        let w = VectorWeights::of(lex(&common_blocks(1010, 100, None)));
        assert!(w.word_is_seed("common"));
        // targets follow the same rule with indeg and L
        let w = VectorWeights::of(lex(&common_blocks(1009, 0, Some(1009))));
        let t = id("c-00000");
        assert_eq!((w.corpus_links(), w.indeg(&t)), (1009, 1009));
        assert!(!w.target_is_seed(&t));
        assert_eq!(w.target_weight(&t).unwrap(), Some(idf(1009, 1009)));
        assert_eq!(w.link(Doc::Block(&n0)).unwrap(), vec![(&t, 1.0)]);
        let w = VectorWeights::of(lex(&common_blocks(1009, 0, Some(100))));
        assert!(w.target_is_seed(&t));
        assert!(!w.target_is_seed(&id("c-00001")) && !w.word_is_seed("absent"));
    }

    fn small() -> Vec<Block> {
        vec![
            blk(
                "c-00001",
                "claim",
                None,
                "Alpha beta gamma.",
                vec![],
                vec![],
            ),
            blk(
                "c-00002",
                "claim",
                None,
                "Beta delta {{c-00001}}.",
                vec![],
                vec![("implements", "c-00003")],
            ),
            blk("c-00003", "claim", None, "Epsilon zeta.", vec![], vec![]),
        ]
    }

    #[test]
    fn vxs1_reads_compute_only_requested_idf_and_candidate_norms() {
        let w = VectorWeights::of(lex(&small()));
        assert_eq!(w.stats(), WeightStats::default());
        assert!(w.idf("beta").unwrap().is_some());
        assert_eq!((w.stats().idf, w.stats().lexical_norms), (1, 0));
        // a candidate's norm reads the idf of its own words alone
        let c1 = id("c-00001");
        w.lexical_norm(Doc::Block(&c1)).unwrap();
        let s = w.stats();
        assert_eq!(
            (s.idf, s.lexical_norms, s.targets, s.link_norms),
            (3, 1, 0, 0)
        );
        // once: a second read computes nothing
        w.lexical(Doc::Block(&c1)).unwrap();
        w.lexical_norm(Doc::Block(&c1)).unwrap();
        assert_eq!(w.stats(), s);
        let c2 = id("c-00002");
        w.link_norm(Doc::Block(&c2)).unwrap();
        let s = w.stats();
        assert_eq!(
            (s.idf, s.lexical_norms, s.targets, s.link_norms),
            (3, 1, 2, 1)
        );
        // a word no block holds computes nothing
        assert_eq!(w.idf("omega").unwrap(), None);
        assert_eq!(w.stats(), s);
        // through a read state: a write and its publish compute no weight
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        write(&mut r, "work", doc_ops("alpha", 3));
        land(&mut r, "work");
        r.read().lexicon(&Scope::Canon).unwrap();
        write(&mut r, "work", doc_ops("beta", 2));
        let st = r.read();
        let work = Scope::View("work".into());
        assert!(st.built(&work).unwrap().weights.get().is_none());
        let w = st.vector_weights(WeightScope::Read(&work)).unwrap();
        assert_eq!(w.stats(), WeightStats::default());
    }

    #[test]
    fn vxs1_lazy_cache_charge_never_installs_partial_norms() {
        assert_eq!(WEIGHT_CACHE_BYTES, 256 * 1024 * 1024);
        let w = VectorWeights::of(lex(&small()));
        assert_eq!(w.charge_limit(), WEIGHT_CACHE_BYTES);
        // the charge of c-00001's norm: the norm entry and its three words' idf entries
        let c1 = id("c-00001");
        let exact = BLOCK_ENTRY_BYTES
            + c1.as_str().len()
            + 3 * WORD_ENTRY_BYTES
            + "alpha".len()
            + "beta".len()
            + "gamma".len();
        let tight = VectorWeights::of(lex(&small())).with_charge_limit(exact - 1);
        assert_eq!(tight.lexical_norm(Doc::Block(&c1)), Err(WeightLimit));
        assert_eq!(tight.lexical(Doc::Block(&c1)), Err(WeightLimit));
        assert_eq!(tight.stats(), WeightStats::default(), "nothing installed");
        // a smaller read still fits, and is charged exactly
        assert!(tight.idf("beta").unwrap().is_some());
        let one = WeightStats {
            idf: 1,
            charged: WORD_ENTRY_BYTES + "beta".len(),
            ..WeightStats::default()
        };
        assert_eq!(tight.stats(), one);
        // the norm still needs `exact` in all: refused, and `alpha` and `gamma` stay out
        assert_eq!(tight.lexical_norm(Doc::Block(&c1)), Err(WeightLimit));
        assert_eq!(tight.stats(), one);
        // at the limit it fits; a cached idf is charged once
        let fits = VectorWeights::of(lex(&small())).with_charge_limit(exact);
        assert!(fits.idf("beta").unwrap().is_some());
        assert!(fits.lexical_norm(Doc::Block(&c1)).unwrap().is_some());
        assert_eq!(fits.stats().charged, exact);
        assert_eq!(fits.stats().lexical_norms, 1);
        // a link norm that does not fit installs neither the norm nor its target weights
        let c2 = id("c-00002");
        let full = fits.stats();
        assert_eq!(fits.link_norm(Doc::Block(&c2)), Err(WeightLimit));
        assert_eq!(fits.stats(), full);
        // the same values as unbounded weights
        assert_eq!(
            fits.lexical(Doc::Block(&c1)).unwrap(),
            w.lexical(Doc::Block(&c1)).unwrap()
        );
    }
}
