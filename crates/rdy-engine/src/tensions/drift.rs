//! Restatement drift (FW-CH2, D-S46DE, D-SES4J; ledger Q12): a change edits a block whose
//! previous text closely matched another block's text, and the edit touches the text they share.
//!
//! The rule is closed. A block takes part when its type has the `normative` or `informative`
//! trait, it is not a decision, and its body has at least [`DRIFT_MIN_WORDS`] words. A body is
//! compared as a set of three-word shingles ([`shingles`]); two sets match at
//! [`similarity_pct`]. Block `x` edited from text `p` to text `n` drifts from another block `y`
//! that takes part when `similarity_pct(S(p), S(y))` is at or above the pack's
//! `drift_threshold` (default 60) and some shingle that `p` and `y` share is not in `n`.
//!
//! A drift resolves only when `y` is edited or the tension is dismissed (D-S46DE); a further
//! edit of `x` keeps it (orchestrator ruling, 2026-10-10). In flight the view's net edit is
//! judged: `p` is `x`'s text where the view's chain of changes on `x` started (the first base
//! among them), `n` the live write's body, and `y` any block of the in-flight materialization
//! that no live write of the same change touches; one tension per (change, `x`, `y`). In canon
//! (FW-CH6a, D-84QSN) `p` is `x`'s text when `y` last changed (as of `y`'s newest landing; `x`'s
//! first text when `x` is younger), `n` its canon body, and `y` a canon block whose newest
//! landing is strictly earlier than `x`'s. It is advisory by default and never softens.
//!
//! Candidates come from an inverted index (shingle to blocks) probed with a prefix filter: a `y`
//! at or above the threshold shares at least `ceil(t * |p| / 100)` shingles with `p`, so it
//! holds one of any `|p| - ceil(t * |p| / 100) + 1` of them; the rarest are probed. Version
//! texts are hashed once and kept by version across carries ([`TextCache`]).
use super::index::has_trait;
use super::{DetectCx, DetectScope, Detector, Found, TensionKind, Write};
use crate::RdyError;
use rdy_model::{Block, BlockId, Hash, decision};
use rdy_schema::{Pack, Trait};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The fewest words a body needs to take part in restatement drift.
pub const DRIFT_MIN_WORDS: usize = 8;

/// This kind's detector.
pub struct RestatementDrift;

/// A landing's (`at_ms`, landing id); `None` sorts before every landing.
type LandedAt = Option<(i64, String)>;

/// A body as drift reads it: its word count and its shingles, sorted, each once.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Text {
    pub words: usize,
    pub set: Vec<u64>,
}

impl Text {
    /// Whether the body is long enough to take part.
    fn long(&self) -> bool {
        self.words >= DRIFT_MIN_WORDS
    }
}

/// The [`Text`] of each version drift has read, by version (a version's text never changes),
/// shared by the clones that hold it.
#[derive(Clone, Debug, Default)]
pub struct TextCache(Arc<Mutex<HashMap<Hash, Arc<Text>>>>);

impl TextCache {
    pub(crate) fn get(&self, v: &Hash) -> Option<Arc<Text>> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(v)
            .cloned()
    }

    fn put(&self, v: Hash, t: Text) -> Arc<Text> {
        Arc::clone(
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(v)
                .or_insert_with(|| Arc::new(t)),
        )
    }
}

/// The [`Text`] of `body`: lowercased, split on every character that is not a letter or a digit,
/// empty words dropped; each run of three consecutive words is one shingle.
pub fn text_of(body: &str) -> Text {
    let lower = body.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut buf = Vec::with_capacity(64);
    let mut set: Vec<u64> = words
        .windows(3)
        .map(|w| shingle(&mut buf, w[0], w[1], w[2]))
        .collect();
    set.sort_unstable();
    set.dedup();
    Text {
        words: words.len(),
        set,
    }
}

/// One shingle's hash: the first 8 bytes, read little-endian, of
/// `Hash::of("rdy-shingle\0" + the three words joined by one space)`. `buf` is scratch space.
fn shingle(buf: &mut Vec<u8>, a: &str, b: &str, c: &str) -> u64 {
    buf.clear();
    buf.extend_from_slice(b"rdy-shingle\0");
    buf.extend_from_slice(a.as_bytes());
    buf.push(b' ');
    buf.extend_from_slice(b.as_bytes());
    buf.push(b' ');
    buf.extend_from_slice(c.as_bytes());
    let h = Hash::of(buf);
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.0[..8]);
    u64::from_le_bytes(first)
}

/// The shingles of `text`: each run of three consecutive words, hashed (see [`text_of`]).
pub fn shingles(text: &str) -> BTreeSet<u64> {
    text_of(text).set.into_iter().collect()
}

/// `|A ∩ B| * 100 / |A ∪ B|`, rounded down; 0 when both are empty.
pub fn similarity_pct(a: &BTreeSet<u64>, b: &BTreeSet<u64>) -> u32 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let shared = small.iter().filter(|s| large.contains(s)).count() as u64;
    let union = (a.len() + b.len()) as u64 - shared;
    if union == 0 {
        return 0;
    }
    (shared * 100 / union) as u32
}

/// How many shingles two sorted sets share (a merge).
fn shared(a: &[u64], b: &[u64]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// [`similarity_pct`] of two sorted sets.
fn similarity(a: &[u64], b: &[u64]) -> u32 {
    let n = shared(a, b) as u64;
    let union = (a.len() + b.len()) as u64 - n;
    if union == 0 {
        return 0;
    }
    (n * 100 / union) as u32
}

/// How many of a set of `len` shingles a prefix filter probes at threshold `t`: any
/// `len - ceil(t * len / 100) + 1` of them meet every set that matches at `t` or more (a match
/// shares at least `ceil(t * len / 100)`). All of them at `t` = 0; none above 100.
fn probe_len(len: usize, t: u32) -> usize {
    let need = (len * t as usize).div_ceil(100);
    (len + 1).saturating_sub(need).min(len)
}

/// Whether sorted sets `a` and `b` can match at `t` or more, a cheap filter that never drops a
/// match. A match shares at least `k = ceil(t * max(|a|, |b|) / 100)` shingles (at least 1), so
/// the smaller is at least `k` long, and (the prefix filtering principle, in value order) the
/// smallest shared shingle is among the first `|a| - k + 1` of `a` and the first `|b| - k + 1`
/// of `b`.
fn may_match(a: &[u64], b: &[u64], t: u32) -> bool {
    let k = (a.len().max(b.len()) * t as usize).div_ceil(100).max(1);
    if k > a.len().min(b.len()) {
        return false;
    }
    shared(&a[..a.len() - k + 1], &b[..b.len() - k + 1]) > 0
}

/// Whether `b`'s type takes part: it has the `normative` or `informative` trait and is not a
/// decision. The body's length is checked apart.
pub(crate) fn restating_type(pack: &Pack, b: &Block) -> bool {
    b.block_type != decision::DECISION
        && (has_trait(pack, b, Trait::Normative) || has_trait(pack, b, Trait::Informative))
}

/// Whether text `p` (its shingles), edited to `n`, drifts from `y`: they match at `threshold` or
/// more, and some shingle `p` and `y` share is not in `n`.
fn drifts(p: &[u64], y: &[u64], n: &[u64], threshold: u32) -> bool {
    similarity(p, y) >= threshold
        && p.iter()
            .any(|s| y.binary_search(s).is_ok() && n.binary_search(s).is_err())
}

/// An inverted index over the blocks of one materialization that take part: each block's
/// text, and the blocks that hold each shingle.
#[derive(Debug, Default)]
pub struct ShingleIndex {
    sets: HashMap<BlockId, Arc<Text>>,
    holders: HashMap<u64, Vec<BlockId>>,
}

impl ShingleIndex {
    /// The index of the blocks in `blocks` that take part. `known` gives a block's text when it
    /// was already read (so it is not hashed again).
    pub(crate) fn of<'b>(
        pack: &Pack,
        blocks: impl IntoIterator<Item = &'b Block>,
        known: impl Fn(&Block) -> Option<Arc<Text>>,
    ) -> Self {
        let mut ix = ShingleIndex::default();
        for b in blocks {
            if !restating_type(pack, b) {
                continue;
            }
            let t = known(b).unwrap_or_else(|| Arc::new(text_of(&b.body)));
            if !t.long() {
                continue;
            }
            for s in &t.set {
                ix.holders.entry(*s).or_default().push(b.id.clone());
            }
            ix.sets.insert(b.id.clone(), t);
        }
        ix
    }

    /// `b`'s text, when it takes part.
    pub fn text_of(&self, b: &BlockId) -> Option<&Arc<Text>> {
        self.sets.get(b)
    }

    /// The blocks that hold shingle `s`.
    pub fn holders(&self, s: u64) -> &[BlockId] {
        self.holders.get(&s).map_or(&[][..], Vec::as_slice)
    }

    /// The blocks that may match `p` at `t` or more: the holders of `p`'s [`probe_len`] rarest
    /// shingles (fewest holders first, then by value). A superset of the matches.
    pub fn probe(&self, p: &[u64], t: u32) -> BTreeSet<&BlockId> {
        let mut counted: Vec<(usize, u64)> =
            p.iter().map(|s| (self.holders(*s).len(), *s)).collect();
        counted.sort_unstable();
        counted
            .iter()
            .take(probe_len(p.len(), t))
            .flat_map(|(_, s)| self.holders(*s))
            .collect()
    }
}

/// The text of version `v` of block `b`: from the cache, else from canon's index or
/// materialization when `v` is canon's version, else from a write of `v`, else from the store.
fn version_text(cx: &DetectCx<'_>, b: &BlockId, v: &Hash) -> Result<Arc<Text>, RdyError> {
    let cache = cx.writes.texts();
    if let Some(t) = cache.get(v) {
        return Ok(t);
    }
    let canon = cx.canon_m()?;
    if canon.version_of(b) == Some(*v) {
        if let Some(ix) = cx.canon_shingles_if_built()
            && let Some(t) = ix.text_of(b)
        {
            return Ok(Arc::clone(t));
        }
        if let Some(block) = canon.get(b) {
            return Ok(cache.put(*v, text_of(&block.body)));
        }
    }
    if let Some(block) = cx
        .writes
        .on(b)
        .iter()
        .find(|w| w.version == Some(*v))
        .and_then(|w| w.new.as_deref())
    {
        return Ok(cache.put(*v, text_of(&block.body)));
    }
    let body = cx.st.reader().get_block(v)?.body;
    Ok(cache.put(*v, text_of(&body)))
}

/// `y`'s text in the scope's materialization, when it takes part there.
fn scope_text(cx: &DetectCx<'_>, y: &BlockId) -> Result<Option<Arc<Text>>, RdyError> {
    let m = cx.inflight_m()?;
    let (Some(block), Some(v)) = (m.get(y), m.version_of(y)) else {
        return Ok(None);
    };
    if !restating_type(cx.st.pack(), block) {
        return Ok(None);
    }
    let t = version_text(cx, y, &v)?;
    Ok(t.long().then_some(t))
}

/// The blocks the scope moved away from canon that take part, with their texts, and, for a run
/// that probes many edits, the moved blocks that hold each shingle (by position).
struct Moved {
    blocks: Vec<(BlockId, Arc<Text>)>,
    holders: Option<HashMap<u64, Vec<usize>>>,
}

/// From this many edits on, a run indexes the moved blocks instead of scanning them per edit.
const MOVED_INDEX_FROM: usize = 16;

impl Moved {
    /// The moved blocks of `cx`'s scope (none in canon), indexed when `edits` is large.
    fn of(cx: &DetectCx<'_>, edits: usize) -> Result<Moved, RdyError> {
        let mut blocks = vec![];
        for (y, v) in cx.moved_blocks()?.iter() {
            let yt = version_text(cx, y, v)?;
            if yt.long() {
                blocks.push((y.clone(), yt));
            }
        }
        let holders = (edits >= MOVED_INDEX_FROM).then(|| {
            let mut h: HashMap<u64, Vec<usize>> = HashMap::new();
            for (i, (_, yt)) in blocks.iter().enumerate() {
                for s in &yt.set {
                    h.entry(*s).or_default().push(i);
                }
            }
            h
        });
        Ok(Moved { blocks, holders })
    }

    /// The moved blocks that may match `p` at `t` or more.
    fn probe<'m>(&'m self, p: &[u64], t: u32) -> Vec<&'m (BlockId, Arc<Text>)> {
        let Some(h) = &self.holders else {
            return self
                .blocks
                .iter()
                .filter(|(_, yt)| may_match(p, &yt.set, t))
                .collect();
        };
        let none: &[usize] = &[];
        let held = |s: &u64| h.get(s).map_or(none, Vec::as_slice);
        let mut counted: Vec<(usize, u64)> = p.iter().map(|s| (held(s).len(), *s)).collect();
        counted.sort_unstable();
        let at: BTreeSet<usize> = counted
            .iter()
            .take(probe_len(p.len(), t))
            .flat_map(|(_, s)| held(s).iter().copied())
            .collect();
        at.into_iter().map(|i| &self.blocks[i]).collect()
    }
}

/// The blocks of the scope's materialization other than `x` that take part and may match `p`,
/// with their texts: canon's blocks the scope did not move (from canon's index) and the moved
/// ones.
fn restating(
    cx: &DetectCx<'_>,
    moved: &Moved,
    x: &BlockId,
    p: &[u64],
    t: u32,
) -> Result<Vec<(BlockId, Arc<Text>)>, RdyError> {
    let canon = cx.canon_shingles()?;
    let mut out = vec![];
    let inflight = cx.scope() == DetectScope::Inflight;
    let (m, canon_m) = (cx.inflight_m()?, cx.canon_m()?);
    for y in canon.probe(p, t) {
        if y == x || (inflight && m.version_of(y) != canon_m.version_of(y)) {
            continue;
        }
        if let Some(yt) = canon.text_of(y) {
            out.push((y.clone(), Arc::clone(yt)));
        }
    }
    for (y, yt) in moved.probe(p, t) {
        if y != x {
            out.push((y.clone(), Arc::clone(yt)));
        }
    }
    Ok(out)
}

/// The version `w`'s view started its chain of changes on `w.block` from: `w`'s base, followed
/// back through each write on the block, of a change that shares a view with `w`'s, that wrote
/// it (never one change twice). `None` when the chain starts with a create, or `w` has no base.
fn chain_start(cx: &DetectCx<'_>, w: &Write) -> Option<Hash> {
    let mut base = w.base?;
    let mut used = vec![w.change];
    loop {
        let before = cx.writes.on(&w.block).iter().find(|o| {
            o.version == Some(base)
                && !used.contains(&o.change)
                && cx.writes.common_view(&o.change, &w.change)
        });
        let Some(o) = before else {
            return Some(base);
        };
        used.push(o.change);
        base = o.base?;
    }
}

/// One live edit as drift judges it: the write, its view's starting text `p` (8 words or more)
/// and its new text `n`.
struct Edit {
    w: Arc<Write>,
    p: Arc<Text>,
    n: Arc<Text>,
}

/// `w` as an [`Edit`], when it is a live update of a type that takes part whose view started
/// from a text of 8 words or more.
fn edit_of(cx: &DetectCx<'_>, w: &Arc<Write>) -> Result<Option<Edit>, RdyError> {
    let (true, Some(new), Some(v)) = (w.live, w.new.as_deref(), w.version) else {
        return Ok(None);
    };
    if !restating_type(cx.st.pack(), new) {
        return Ok(None);
    }
    let Some(start) = chain_start(cx, w) else {
        return Ok(None);
    };
    let p = version_text(cx, &w.block, &start)?;
    if !p.long() {
        return Ok(None);
    }
    let n = version_text(cx, &w.block, &v)?;
    Ok(Some(Edit {
        w: Arc::clone(w),
        p,
        n,
    }))
}

/// The drifts found, by (`x`, change, `y`), each with its write.
type Pairs = BTreeMap<(BlockId, Hash, BlockId), Arc<Write>>;

impl RestatementDrift {
    /// The in-flight form. A full run checks every live edit. A run around `seeds` checks the
    /// live edits of the seeds (as `x`) and, for each seed that takes part, every live edit whose
    /// starting text the seed may restate (the seed as `y`).
    fn inflight(
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let t = cx.st.pack().drift_threshold();
        let mut pairs = Pairs::new();
        // the seeds' edits (or all of them), as x
        let mut as_x = vec![];
        match seeds {
            Some(seeds) => {
                for s in seeds {
                    for w in cx.writes.on(s) {
                        as_x.extend(edit_of(cx, w)?);
                    }
                }
            }
            None => {
                for w in cx.writes.live() {
                    as_x.extend(edit_of(cx, w)?);
                }
            }
        }
        let moved = if as_x.is_empty() {
            Moved {
                blocks: vec![],
                holders: None,
            }
        } else {
            Moved::of(cx, as_x.len())?
        };
        for e in &as_x {
            for (y, yt) in restating(cx, &moved, &e.w.block, &e.p.set, t)? {
                if !cx.writes.live_write_of(&e.w.change, &y)
                    && drifts(&e.p.set, &yt.set, &e.n.set, t)
                {
                    pairs.insert((e.w.block.clone(), e.w.change, y), Arc::clone(&e.w));
                }
            }
        }
        // the seeds as y, against every live edit
        if let Some(seeds) = seeds {
            let mut edits: Option<Vec<Edit>> = None;
            for y in seeds {
                let Some(yt) = scope_text(cx, y)? else {
                    continue;
                };
                if edits.is_none() {
                    let mut all = vec![];
                    for w in cx.writes.live() {
                        all.extend(edit_of(cx, w)?);
                    }
                    edits = Some(all);
                }
                for e in edits.iter().flatten() {
                    if e.w.block != *y
                        && !cx.writes.live_write_of(&e.w.change, y)
                        && may_match(&e.p.set, &yt.set, t)
                        && drifts(&e.p.set, &yt.set, &e.n.set, t)
                    {
                        pairs.insert((e.w.block.clone(), e.w.change, y.clone()), Arc::clone(&e.w));
                    }
                }
            }
        }
        for ((x, change, y), w) in pairs {
            let Some((side, version)) = cx.side_at(&y)? else {
                continue;
            };
            out.push(Found {
                kind: TensionKind::RestatementDrift,
                summary: format!(
                    "{x} (change {}) changed text that {y} restates",
                    change.short()
                ),
                stable: cx.stable(&y)?,
                involved: vec![cx.of_write(&w), cx.involved(side, &y, version)?],
            });
        }
        Ok(())
    }

    /// The canon form: for each canon block `x` of a type that takes part, each older version
    /// `p` of it against the canon blocks that restate `p` and whose newest landing falls while
    /// `p` was `x`'s text (or before `x`'s first text), strictly before `x`'s newest landing.
    fn canon(cx: &DetectCx<'_>, out: &mut Vec<Found>) -> Result<(), RdyError> {
        let pack = cx.st.pack();
        let t = pack.drift_threshold();
        let m = cx.canon_m()?;
        let mut steps = cx.st.canon_steps()?;
        // each block's newest landing; the change and base of its canon version
        let mut newest: HashMap<BlockId, LandedAt> = HashMap::new();
        let mut edited: Vec<(BlockId, Hash, LandedAt, Hash)> = vec![];
        let mut landings: Vec<LandedAt> = vec![];
        for b in m.iter().filter(|b| restating_type(pack, b)) {
            let (c, step) = steps.current(&b.id)?;
            if text_of_len(&b.body) >= DRIFT_MIN_WORDS {
                landings.push(step.landed_at.clone());
            }
            if let Some(base) = step.base {
                edited.push((b.id.clone(), c, step.landed_at.clone(), base));
            }
            newest.insert(b.id.clone(), step.landed_at);
        }
        landings.sort();
        // no y can be older than the oldest landing among them
        let Some(floor) = landings.first().cloned() else {
            return Ok(());
        };
        let any_in = |lo: Option<&LandedAt>, hi: &LandedAt| {
            let from = lo.map_or(0, |lo| landings.partition_point(|l| l < lo));
            landings.get(from).is_some_and(|l| l < hi)
        };
        let moved = Moved {
            blocks: vec![],
            holders: None,
        };
        let mut found: Vec<(BlockId, BlockId)> = vec![];
        for (x, c, x_landed, base) in edited {
            if x_landed <= floor {
                continue;
            }
            let Some(xb) = m.get(&x) else { continue };
            let n = text_of(&xb.body);
            let mut used: HashSet<Hash> = HashSet::from([c]);
            let mut hi = x_landed;
            let mut version = Some(base);
            while let Some(v) = version {
                let Some((c, step)) = steps.step(&v, &x, &used)? else {
                    break;
                };
                used.insert(c);
                // the landings p was x's text through: [its landing, hi); the first text's from
                // before every landing
                let lo = step.base.is_some().then_some(&step.landed_at);
                if any_in(lo, &hi) {
                    let p = older_text(cx, &steps, &v)?;
                    if p.long() {
                        for (y, yt) in restating(cx, &moved, &x, &p.set, t)? {
                            let y_landed = newest.get(&y).cloned().flatten();
                            if lo.is_none_or(|lo| &y_landed >= lo)
                                && y_landed < hi
                                && drifts(&p.set, &yt.set, &n.set, t)
                            {
                                found.push((x.clone(), y));
                            }
                        }
                    }
                }
                if step.landed_at < hi {
                    hi = step.landed_at;
                }
                if hi <= floor {
                    break;
                }
                version = step.base;
            }
        }
        for (x, y) in found {
            let (Some((xs, xv)), Some((ys, yv))) = (cx.canon_side(&x)?, cx.canon_side(&y)?) else {
                continue;
            };
            out.push(Found {
                kind: TensionKind::RestatementDrift,
                summary: format!("{x} (canon) changed text that {y} restates"),
                stable: cx.stable(&y)?,
                involved: vec![
                    cx.involved(xs, &x, Some(xv))?,
                    cx.involved(ys, &y, Some(yv))?,
                ],
            });
        }
        Ok(())
    }
}

/// The number of words in `body` (see [`text_of`]), without hashing.
fn text_of_len(body: &str) -> usize {
    body.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .count()
}

/// The text of an older canon version `v`: from the cache, else read with `steps`.
fn older_text(
    cx: &DetectCx<'_>,
    steps: &crate::history::CanonSteps<'_>,
    v: &Hash,
) -> Result<Arc<Text>, RdyError> {
    let cache = cx.writes.texts();
    if let Some(t) = cache.get(v) {
        return Ok(t);
    }
    Ok(cache.put(*v, text_of(&steps.body(v)?)))
}

impl Detector for RestatementDrift {
    fn kind(&self) -> TensionKind {
        TensionKind::RestatementDrift
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        match cx.scope() {
            // the canon part is always a full run
            DetectScope::Canon => Self::canon(cx, out),
            DetectScope::Inflight => Self::inflight(cx, seeds, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn cc7_shingles_and_similarity() {
        let a = shingles("The retry limit is five seconds for every job.");
        let b = shingles("the RETRY limit is five seconds, for every job");
        assert_eq!(
            similarity_pct(&a, &b),
            100,
            "case and punctuation do not count"
        );
        let c = shingles("The retry limit is ten seconds for every job.");
        assert!(similarity_pct(&a, &c) < 60, "{}", similarity_pct(&a, &c));
        assert_eq!(similarity_pct(&BTreeSet::new(), &BTreeSet::new()), 0);
    }

    /// The prefix filter keeps every pair that matches: [`may_match`] and [`ShingleIndex::probe`]
    /// find each set at or above the threshold, over pseudo-random sets.
    #[test]
    fn cc7_the_prefix_filter_never_drops_a_match() {
        let mut seed: u64 = 0x5eed;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            seed >> 33
        };
        let sets: Vec<BTreeSet<u64>> = (0..200)
            .map(|_| {
                let len = 1 + next() % 30;
                (0..len).map(|_| next() % 40).collect()
            })
            .collect();
        let mut ix = ShingleIndex::default();
        for (i, set) in sets.iter().enumerate() {
            let id = BlockId::parse(&format!("p-{:05X}", i + 0x10000)).expect("id");
            for s in set {
                ix.holders.entry(*s).or_default().push(id.clone());
            }
            ix.sets.insert(
                id,
                Arc::new(Text {
                    words: set.len() + 2,
                    set: set.iter().copied().collect(),
                }),
            );
        }
        let mut matched = 0;
        for t in [1, 30, 60, 99, 100] {
            for (i, a) in sets.iter().enumerate() {
                let av: Vec<u64> = a.iter().copied().collect();
                let probed = ix.probe(&av, t);
                for (j, b) in sets.iter().enumerate() {
                    let bv: Vec<u64> = b.iter().copied().collect();
                    assert_eq!(similarity(&av, &bv), similarity_pct(a, b));
                    if similarity_pct(a, b) < t {
                        continue;
                    }
                    matched += 1;
                    assert!(may_match(&av, &bv, t), "may_match {i} {j} at {t}");
                    let id = BlockId::parse(&format!("p-{:05X}", j + 0x10000)).expect("id");
                    assert!(probed.contains(&id), "probe {i} {j} at {t}");
                }
            }
        }
        assert!(matched > 1000, "{matched}");
        assert_eq!(probe_len(10, 60), 5);
        assert_eq!(probe_len(10, 100), 1);
        assert_eq!(probe_len(10, 0), 10);
        assert_eq!(probe_len(0, 60), 0);
    }
}
