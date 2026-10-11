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
//! In flight, `p` is the base of each live update and `n` its new body; `y` is any block of the
//! in-flight materialization that no live write of the same change touches. One tension per
//! (change, `x`, `y`). In canon (FW-CH6a, D-84QSN) `p` is the body before each canon block's
//! newest update, `n` its canon body, and `y` a canon block whose newest landing is strictly
//! earlier than `x`'s: a `y` edited in the same or a later landing has moved with `x`, so the
//! tension resolves when `y` changes. It is advisory by default and never softens.
use super::index::{MovedShingles, has_trait};
use super::{DetectCx, DetectScope, Detector, Found, TensionKind, Write};
use crate::RdyError;
use rdy_model::{Block, BlockId, Hash, decision};
use rdy_schema::{Pack, Trait};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// The fewest words a body needs to take part in restatement drift.
pub const DRIFT_MIN_WORDS: usize = 8;

/// This kind's detector.
pub struct RestatementDrift;

/// A set of shingles, shared.
type Shingles = Arc<BTreeSet<u64>>;

/// A landing's (`at_ms`, landing id).
type LandedAt = Option<(i64, String)>;

/// The words of `text`: lowercased, split on every character that is not a letter or a digit,
/// empty words dropped.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// The number of [`words`] in `text`.
fn word_count(text: &str) -> usize {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .count()
}

/// One shingle's hash: the first 8 bytes, read little-endian, of
/// `Hash::of("rdy-shingle\0" + the three words joined by one space)`.
fn shingle(a: &str, b: &str, c: &str) -> u64 {
    let h = Hash::of(format!("rdy-shingle\0{a} {b} {c}").as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.0[..8]);
    u64::from_le_bytes(first)
}

/// The shingles of `text`: each run of three consecutive [`words`], hashed.
pub fn shingles(text: &str) -> BTreeSet<u64> {
    words(text)
        .windows(3)
        .map(|w| shingle(&w[0], &w[1], &w[2]))
        .collect()
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

/// Whether `b`'s type takes part: it has the `normative` or `informative` trait and is not a
/// decision. The body's length is checked apart.
pub(crate) fn restating_type(pack: &Pack, b: &Block) -> bool {
    b.block_type != decision::DECISION
        && (has_trait(pack, b, Trait::Normative) || has_trait(pack, b, Trait::Informative))
}

/// The shingles of `b`'s body when `b` takes part: its type does and its body has at least
/// [`DRIFT_MIN_WORDS`] words.
fn taking_part(pack: &Pack, b: &Block) -> Option<BTreeSet<u64>> {
    (restating_type(pack, b) && word_count(&b.body) >= DRIFT_MIN_WORDS).then(|| shingles(&b.body))
}

/// The shingles of a previous text `p`, when it has at least [`DRIFT_MIN_WORDS`] words.
fn previous_shingles(p: &str) -> Option<BTreeSet<u64>> {
    (word_count(p) >= DRIFT_MIN_WORDS).then(|| shingles(p))
}

/// Whether text `p` (its shingles), edited to `n`, drifts from `y`: they match at `threshold` or
/// more, and some shingle `p` and `y` share is not in `n`.
fn drifts(p: &BTreeSet<u64>, y: &BTreeSet<u64>, n: &BTreeSet<u64>, threshold: u32) -> bool {
    similarity_pct(p, y) >= threshold && p.iter().any(|s| y.contains(s) && !n.contains(s))
}

/// An inverted index over the blocks of one materialization that take part: each block's
/// shingles, and the blocks that hold each shingle.
#[derive(Debug, Default)]
pub struct ShingleIndex {
    sets: HashMap<BlockId, Shingles>,
    holders: HashMap<u64, Vec<BlockId>>,
}

impl ShingleIndex {
    /// The index of the blocks in `blocks` that take part.
    pub(crate) fn of<'b>(pack: &Pack, blocks: impl IntoIterator<Item = &'b Block>) -> Self {
        let mut ix = ShingleIndex::default();
        for b in blocks {
            if let Some(set) = taking_part(pack, b) {
                for s in &set {
                    ix.holders.entry(*s).or_default().push(b.id.clone());
                }
                ix.sets.insert(b.id.clone(), Arc::new(set));
            }
        }
        ix
    }

    /// `b`'s shingles, when it takes part.
    pub fn set_of(&self, b: &BlockId) -> Option<&Shingles> {
        self.sets.get(b)
    }

    /// The blocks that hold shingle `s`.
    pub fn holders(&self, s: u64) -> &[BlockId] {
        self.holders.get(&s).map_or(&[][..], Vec::as_slice)
    }
}

/// `y`'s shingles in the scope's materialization, when it takes part there: from the moved
/// blocks' index when the scope moved `y` away from canon, else from canon's.
fn scope_set(canon: &ShingleIndex, moved: &MovedShingles, y: &BlockId) -> Option<Shingles> {
    if moved.blocks.contains(y) {
        return moved.index.set_of(y).cloned();
    }
    canon.set_of(y).cloned()
}

/// The blocks of the scope's materialization that take part and share a shingle with `p`, with
/// their shingles, by id.
fn restating(cx: &DetectCx<'_>, p: &BTreeSet<u64>) -> Result<Vec<(BlockId, Shingles)>, RdyError> {
    let canon = cx.canon_shingles()?;
    let moved = cx.moved_shingles()?;
    let mut ids: BTreeSet<&BlockId> = BTreeSet::new();
    for s in p {
        ids.extend(canon.holders(*s));
        ids.extend(moved.index.holders(*s));
    }
    Ok(ids
        .into_iter()
        .filter_map(|y| scope_set(&canon, &moved, y).map(|set| (y.clone(), set)))
        .collect())
}

/// The shingles of the text write `w` replaced, when its base has at least [`DRIFT_MIN_WORDS`]
/// words: from canon's index when the base is canon's version, else read from the store.
fn base_set(cx: &DetectCx<'_>, w: &Write) -> Result<Option<Shingles>, RdyError> {
    let Some(base) = w.base else {
        return Ok(None);
    };
    if cx.canon_m()?.version_of(&w.block) == Some(base) {
        return Ok(cx.canon_shingles()?.set_of(&w.block).cloned());
    }
    let p = cx.st.reader().get_block(&base)?;
    Ok(previous_shingles(&p.body).map(Arc::new))
}

/// Whether a live write of change `c` touches `y`.
fn touched_by(cx: &DetectCx<'_>, c: &Hash, y: &BlockId) -> bool {
    cx.writes.live_on(y).iter().any(|w| w.change == *c)
}

/// An edit's previous and new shingles.
type EditSets = (Shingles, Shingles);

impl RestatementDrift {
    /// The in-flight form. A full run checks every live update of a type that takes part. A run
    /// around `seeds` checks the live updates of the seeds (as `x`) and, for each seed that takes
    /// part, every live update whose previous text the seed may restate (the seed as `y`).
    fn inflight(
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let pack = cx.st.pack();
        let threshold = pack.drift_threshold();
        let edits: Vec<Arc<Write>> = cx
            .writes
            .live()
            .filter(|w| {
                w.base.is_some() && w.new.as_deref().is_some_and(|b| restating_type(pack, b))
            })
            .cloned()
            .collect();
        if edits.is_empty() {
            return Ok(());
        }
        // each edit's previous and new shingles, read once
        let mut memo: HashMap<usize, Option<EditSets>> = HashMap::new();
        let mut sets_of = |i: usize| -> Result<Option<EditSets>, RdyError> {
            if let Some(s) = memo.get(&i) {
                return Ok(s.clone());
            }
            let w = &edits[i];
            let s = base_set(cx, w)?.map(|p| {
                let n = w
                    .new
                    .as_deref()
                    .map_or_else(BTreeSet::new, |b| shingles(&b.body));
                (p, Arc::new(n))
            });
            memo.insert(i, s.clone());
            Ok(s)
        };
        let mut pairs: BTreeSet<(usize, BlockId)> = BTreeSet::new();
        for (i, w) in edits.iter().enumerate() {
            if seeds.is_some_and(|s| !s.contains(&w.block)) {
                continue;
            }
            let Some((p, n)) = sets_of(i)? else {
                continue;
            };
            for (y, ys) in restating(cx, &p)? {
                if y != w.block && !touched_by(cx, &w.change, &y) && drifts(&p, &ys, &n, threshold)
                {
                    pairs.insert((i, y));
                }
            }
        }
        if let Some(seeds) = seeds {
            let canon = cx.canon_shingles()?;
            let moved = cx.moved_shingles()?;
            for y in seeds {
                let Some(ys) = scope_set(&canon, &moved, y) else {
                    continue;
                };
                for (i, w) in edits.iter().enumerate() {
                    if w.block == *y || touched_by(cx, &w.change, y) {
                        continue;
                    }
                    let Some((p, n)) = sets_of(i)? else {
                        continue;
                    };
                    if drifts(&p, &ys, &n, threshold) {
                        pairs.insert((i, y.clone()));
                    }
                }
            }
        }
        for (i, y) in pairs {
            let w = &edits[i];
            let Some((side, version)) = cx.side_at(&y)? else {
                continue;
            };
            out.push(Found {
                kind: TensionKind::RestatementDrift,
                summary: format!(
                    "{} (change {}) changed text that {y} restates",
                    w.block,
                    w.change.short()
                ),
                stable: cx.stable(&y)?,
                involved: vec![cx.of_write(w), cx.involved(side, &y, version)?],
            });
        }
        Ok(())
    }

    /// The canon form: every canon block whose newest version is an update, against the canon
    /// blocks that restate its previous text and whose newest landing is strictly earlier.
    fn canon(cx: &DetectCx<'_>, out: &mut Vec<Found>) -> Result<(), RdyError> {
        let pack = cx.st.pack();
        let threshold = pack.drift_threshold();
        let m = cx.canon_m()?;
        let ids: Vec<BlockId> = m
            .iter()
            .filter(|b| restating_type(pack, b))
            .map(|b| b.id.clone())
            .collect();
        let previous = cx.st.previous_texts(&ids)?;
        let mut candidates: Vec<(BlockId, BlockId, LandedAt)> = vec![];
        for (x, pt) in previous {
            let (Some(p), Some(xb)) = (previous_shingles(&pt.body), m.get(&x)) else {
                continue;
            };
            let n = shingles(&xb.body);
            for (y, ys) in restating(cx, &p)? {
                if y != x && drifts(&p, &ys, &n, threshold) {
                    candidates.push((x.clone(), y, pt.landed_at.clone()));
                }
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }
        let ys: Vec<BlockId> = candidates.iter().map(|(_, y, _)| y.clone()).collect();
        let landed: BTreeMap<BlockId, LandedAt> = cx.st.canon_landed_at(&ys)?;
        for (x, y, x_landed) in candidates {
            // strictly earlier; no known landing sorts before every landing
            if landed.get(&y).cloned().flatten() >= x_landed {
                continue;
            }
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
}
