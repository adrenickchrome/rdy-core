//! Semantic tension (plan M2e1; D-2K8Q5, D-VA8DA, D-WMCFW): a reviewer's `tension` verdict on
//! a pair of blocks at given contents, read from the semantic state's records. Each block of
//! the pair is a side wherever it holds the judged content: every live in-flight write of that
//! content (a change side), and canon when canon holds that content. Canon is resolved per view
//! (final ruling 2): a canon side pairs with a change side whose view does not touch the canon
//! side's block, so another view's edit of that block leaves the form alone. Records are read by
//! the contents the blocks hold now, never all of a block's. So a content change resolves the
//! tension, a repin or a tracking-field edit
//! keeps it (the content hash leaves them out), and a landing re-keys it (same vkey, a canon
//! side). With every side canon it stays open in canon (F6). It is advisory; it blocks only
//! when its latest version is a person's acceptance of the reviewer's advice.
use super::index::DetectCx;
use super::{
    DetectScope, Detector, Found, Involved, SideRef, Tension, TensionKind, TensionSeverity,
};
use crate::{Materialized, RdyError};
use rdy_model::{BlockId, Hash, PairItem, pair_key};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub struct Semantic;

thread_local! {
    /// This thread's count of semantic tension records the detector visited.
    static RECORDS_VISITED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test hook: this thread's count of semantic tension records the detector visited (each record
/// once per detection run). A test reads the difference over a call.
#[doc(hidden)]
pub fn records_visited() -> u64 {
    RECORDS_VISITED.with(std::cell::Cell::get)
}

/// The pair key of `involved`'s two blocks and their contents, when it has exactly two
/// distinct blocks, each at one content.
fn key_of(involved: &[Involved]) -> Option<Hash> {
    let mut items: BTreeSet<PairItem> = BTreeSet::new();
    for i in involved {
        items.insert(PairItem {
            block: i.block.clone(),
            content: i.content?,
        });
    }
    let blocks: BTreeSet<&BlockId> = items.iter().map(|i| &i.block).collect();
    (items.len() == 2 && blocks.len() == 2)
        .then(|| pair_key(&items.into_iter().collect::<Vec<_>>()))
}

/// The pair key of semantic tension `t`: that of its two involved blocks and their contents.
/// `None` for a tension of another kind, even one on a judged pair (a premise shift between the
/// pair's blocks), so the semantic code never takes another kind's tension for the pair's.
pub fn pair_of(t: &Tension) -> Option<Hash> {
    if t.kind != TensionKind::Semantic {
        return None;
    }
    key_of(&t.involved)
}

/// What one detection run reads about a block, once: its live in-flight writes by content (in
/// flight), canon's content, and canon's side, read from the store only when an item's content
/// matches canon's.
struct BlockSides {
    live: BTreeMap<Hash, Vec<Involved>>,
    canon_content: Option<Hash>,
    canon_side: Option<Option<(SideRef, Hash)>>,
}

/// The sides of the items one run visits, memoised per block (see [`BlockSides`]).
struct Sides<'c, 'a> {
    cx: &'c DetectCx<'a>,
    canon: Arc<Materialized>,
    blocks: BTreeMap<BlockId, BlockSides>,
}

impl<'c, 'a> Sides<'c, 'a> {
    fn new(cx: &'c DetectCx<'a>) -> Result<Self, RdyError> {
        Ok(Sides {
            cx,
            canon: cx.canon_m()?,
            blocks: BTreeMap::new(),
        })
    }

    /// What this run reads about `b` (see [`BlockSides`]), read on its first visit.
    fn block(&mut self, b: &BlockId) -> &mut BlockSides {
        if !self.blocks.contains_key(b) {
            let read = self.read(b);
            self.blocks.insert(b.clone(), read);
        }
        self.blocks.get_mut(b).expect("read above")
    }

    fn read(&self, b: &BlockId) -> BlockSides {
        let cx = self.cx;
        let mut live: BTreeMap<Hash, Vec<Involved>> = BTreeMap::new();
        if cx.scope() == DetectScope::Inflight {
            for w in cx.writes.live_on(b) {
                let i = cx.of_write(&w);
                if let Some(c) = i.content {
                    live.entry(c).or_default().push(i);
                }
            }
        }
        BlockSides {
            live,
            canon_content: self.canon.get(b).map(|blk| cx.content_of(blk)),
            canon_side: None,
        }
    }

    /// The contents a side of `b` holds now, in the scope the run reads: its live in-flight
    /// writes' and canon's in flight; canon's in canon.
    fn contents(&mut self, b: &BlockId) -> Vec<Hash> {
        let bs = self.block(b);
        let mut out: Vec<Hash> = bs.live.keys().copied().collect();
        if let Some(c) = bs.canon_content
            && !bs.live.contains_key(&c)
        {
            out.push(c);
        }
        out
    }

    /// The sides of `item` in the scope the run reads: every live in-flight write of the block
    /// that writes the judged content (none in canon), and canon's side when canon holds that
    /// content. Which change a canon side may pair with is the caller's (per view).
    fn of(&mut self, item: &PairItem) -> Result<(Vec<Involved>, Option<Involved>), RdyError> {
        let cx = self.cx;
        let bs = self.block(&item.block);
        let changes: Vec<Involved> = bs.live.get(&item.content).cloned().unwrap_or_default();
        if bs.canon_content != Some(item.content) {
            return Ok((changes, None));
        }
        if bs.canon_side.is_none() {
            bs.canon_side = Some(cx.canon_side(&item.block)?);
        }
        let canon = match &bs.canon_side {
            Some(Some((side, v))) => Some(Involved {
                side: side.clone(),
                block: item.block.clone(),
                version: Some(*v),
                content: Some(item.content),
            }),
            _ => None,
        };
        Ok((changes, canon))
    }
}

/// Whether in-flight side `x` meets block `b` as canon holds it: some view of `x`'s change holds
/// no write of `b` (final ruling 2, as premise shift reads a citer's view). A canon side is not.
fn sees_canon(cx: &DetectCx<'_>, x: &Involved, b: &BlockId) -> bool {
    match &x.side {
        SideRef::Change(c) => cx.writes.untouched_in_a_view_of(c, b),
        _ => false,
    }
}

impl Detector for Semantic {
    fn kind(&self) -> TensionKind {
        TensionKind::Semantic
    }

    fn stored(&self) -> bool {
        true
    }

    /// A repin keeps the content, so it never resolves a semantic tension.
    fn resolve_hint(&self) -> &'static str {
        "change the text of a side, or dismiss it"
    }

    /// Blocking when the pair's latest version is blocking (a person accepted the reviewer's
    /// advice), else advisory: advice alone never changes it.
    fn own_severity(&self, cx: &DetectCx<'_>, f: &Found) -> Option<TensionSeverity> {
        let blocking = key_of(&f.involved)
            .and_then(|k| cx.st.semantic().tension(&k).map(|t| t.record.blocking))
            .unwrap_or(false);
        Some(if blocking {
            TensionSeverity::Blocking
        } else {
            TensionSeverity::Advisory
        })
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let state = cx.st.semantic();
        if !state.has_tensions() {
            return Ok(());
        }
        // the blocks whose records to read: with seeds, the seeds; with none, in flight, the
        // blocks some in-flight change writes (a record on canon blocks only gives an all-canon
        // combination, which the in-flight scope never keeps); in canon, every block with a record
        let blocks: Vec<&BlockId> = match (seeds, cx.scope()) {
            (Some(seeds), _) => seeds.iter().collect(),
            (None, DetectScope::Inflight) => cx.writes.blocks().collect(),
            (None, DetectScope::Canon) => state.blocks().collect(),
        };
        // FW-SW3: only the records on contents both blocks hold now, found by item, so records
        // on contents a block no longer holds are never visited
        let mut sides = Sides::new(cx)?;
        let mut keys: BTreeSet<Hash> = BTreeSet::new();
        for b in blocks {
            if !state.has_block(b) {
                continue;
            }
            for content in sides.contents(b) {
                let item = PairItem {
                    block: b.clone(),
                    content,
                };
                let Some(partners) = state.partners(&item) else {
                    continue;
                };
                for (other, by_content) in partners {
                    for oc in sides.contents(other) {
                        if let Some(k) = by_content.get(&oc) {
                            keys.insert(*k);
                        }
                    }
                }
            }
        }
        for key in keys {
            let Some(t) = state.tension(&key) else {
                continue;
            };
            RECORDS_VISITED.with(|n| n.set(n.get() + 1));
            let [a, b] = t.record.items.as_slice() else {
                continue;
            };
            let (a_changes, a_canon) = sides.of(a)?;
            let (b_changes, b_canon) = sides.of(b)?;
            let mut forms: Vec<(&Involved, &Involved)> = vec![];
            if cx.scope() == DetectScope::Canon {
                if let (Some(x), Some(y)) = (&a_canon, &b_canon) {
                    forms.push((x, y));
                }
            } else {
                for x in &a_changes {
                    for y in &b_changes {
                        forms.push((x, y));
                    }
                }
                // final ruling 2: canon is a side of a block for every form whose other side is
                // a change in a view that does not touch that block
                if let Some(y) = &b_canon {
                    for x in a_changes.iter().filter(|x| sees_canon(cx, x, &b.block)) {
                        forms.push((x, y));
                    }
                }
                if let Some(x) = &a_canon {
                    for y in b_changes.iter().filter(|y| sees_canon(cx, y, &a.block)) {
                        forms.push((x, y));
                    }
                }
            }
            if forms.is_empty() {
                continue;
            }
            let (lo, hi) = if a.block <= b.block {
                (&a.block, &b.block)
            } else {
                (&b.block, &a.block)
            };
            let summary = format!("a reviewer judged that {lo} and {hi} disagree");
            for (x, y) in forms {
                out.push(Found {
                    kind: TensionKind::Semantic,
                    involved: vec![x.clone(), y.clone()],
                    summary: summary.clone(),
                    stable: true,
                });
            }
        }
        Ok(())
    }
}
