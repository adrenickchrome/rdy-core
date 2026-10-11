//! Semantic tension (plan M2e1; D-2K8Q5, D-VA8DA, D-WMCFW): a reviewer's `tension` verdict on
//! a pair of blocks at given contents, read from the semantic state's records. Each block of
//! the pair is a side wherever it holds the judged content: every live in-flight write of that
//! content (a change side), and canon when no in-flight write touches the block and canon holds
//! that content. So a content change resolves the tension, a repin or a tracking-field edit
//! keeps it (the content hash leaves them out), and a landing re-keys it (same vkey, a canon
//! side). With every side canon it stays open in canon (F6). It is advisory; it blocks only
//! when its latest version is a person's acceptance of the reviewer's advice.
use super::index::DetectCx;
use super::{
    DetectScope, Detector, Found, Involved, SideRef, Tension, TensionKind, TensionSeverity,
};
use crate::semantic::state::OpenTension;
use crate::{Materialized, RdyError};
use rdy_model::{BlockId, Hash, PairItem, pair_key};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub struct Semantic;

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
/// flight), whether any in-flight write touches it, canon's content when canon is read, and
/// canon's side, read from the store only when an item's content matches canon's.
struct BlockSides {
    live: BTreeMap<Hash, Vec<Involved>>,
    touched: bool,
    canon_content: Option<Hash>,
    canon_side: Option<Option<(SideRef, Hash)>>,
}

/// The sides of the items one run visits, memoised per block (see [`BlockSides`]): a record on
/// a content its block no longer holds costs a map lookup after the block's first visit.
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

    fn known(&self, b: &BlockId) -> bool {
        self.blocks.contains_key(b)
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
        let mut touched = false;
        if cx.scope() == DetectScope::Inflight {
            for w in cx.writes.live_on(b) {
                let i = cx.of_write(&w);
                if let Some(c) = i.content {
                    live.entry(c).or_default().push(i);
                }
            }
            touched = !cx.writes.on(b).is_empty();
        }
        // canon is read only where no in-flight write touches the block
        let canon_content = if touched {
            None
        } else {
            self.canon.get(b).map(|blk| cx.content_of(blk))
        };
        BlockSides {
            live,
            touched,
            canon_content,
            canon_side: None,
        }
    }

    /// The sides of `item` in the scope the run reads: in flight, every live write of the block
    /// that writes the judged content, and canon when no in-flight write touches the block and
    /// canon holds that content; in canon, canon only.
    fn of(&mut self, item: &PairItem) -> Result<Vec<Involved>, RdyError> {
        let cx = self.cx;
        let bs = self.block(&item.block);
        let mut out: Vec<Involved> = bs.live.get(&item.content).cloned().unwrap_or_default();
        if bs.touched || bs.canon_content != Some(item.content) {
            return Ok(out);
        }
        if bs.canon_side.is_none() {
            bs.canon_side = Some(cx.canon_side(&item.block)?);
        }
        if let Some(Some((side, v))) = &bs.canon_side {
            out.push(Involved {
                side: side.clone(),
                block: item.block.clone(),
                version: Some(*v),
                content: Some(item.content),
            });
        }
        Ok(out)
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
        // with seeds, the records that involve a seed (an index lookup each). With none, in
        // flight, the records on a block some in-flight change writes: a record on canon blocks
        // only gives an all-canon combination, which the in-flight scope never keeps. In canon,
        // every record.
        let around = |seeds: &mut dyn Iterator<Item = &BlockId>| {
            let mut by_key: BTreeMap<Hash, Arc<OpenTension>> = BTreeMap::new();
            for s in seeds {
                for t in state.involving(s) {
                    by_key.entry(t.record.key).or_insert(t);
                }
            }
            by_key.into_values().collect::<Vec<_>>()
        };
        let open: Vec<Arc<OpenTension>> = match (seeds, cx.scope()) {
            (Some(seeds), _) => around(&mut seeds.iter()),
            (None, DetectScope::Inflight) => around(&mut cx.writes.blocks()),
            (None, DetectScope::Canon) => state.tensions().cloned().collect(),
        };
        let mut sides = Sides::new(cx)?;
        for t in open {
            let items = &t.record.items;
            let [a, b] = items.as_slice() else {
                continue;
            };
            // the item whose block this run already read first: a record on a content that
            // block no longer holds is dropped without reading the other block
            let swap = sides.known(&b.block) && !sides.known(&a.block);
            let (first, second) = if swap { (b, a) } else { (a, b) };
            let sf = sides.of(first)?;
            if sf.is_empty() {
                continue;
            }
            let ss = sides.of(second)?;
            if ss.is_empty() {
                continue;
            }
            let (sa, sb) = if swap { (ss, sf) } else { (sf, ss) };
            let (lo, hi) = if a.block <= b.block {
                (&a.block, &b.block)
            } else {
                (&b.block, &a.block)
            };
            let summary = format!("a reviewer judged that {lo} and {hi} disagree");
            for x in &sa {
                for y in &sb {
                    out.push(Found {
                        kind: TensionKind::Semantic,
                        involved: vec![x.clone(), y.clone()],
                        summary: summary.clone(),
                        stable: true,
                    });
                }
            }
        }
        Ok(())
    }
}
