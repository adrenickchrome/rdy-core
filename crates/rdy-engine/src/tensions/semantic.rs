//! Semantic tension (plan M2e1; D-2K8Q5, D-VA8DA, D-WMCFW): a reviewer's `tension` verdict on
//! a pair of blocks at given contents, read from the semantic state's records. Each block of
//! the pair is a side wherever it holds the judged content: every live in-flight write of that
//! content (a change side), and canon when no in-flight write touches the block and canon holds
//! that content. So a content change resolves the tension, a repin or a tracking-field edit
//! keeps it (the content hash leaves them out), and a landing re-keys it (same vkey, a canon
//! side). With every side canon it stays open in canon (F6). It is advisory; it blocks only
//! when its latest version is a person's acceptance of the reviewer's advice.
use super::index::DetectCx;
use super::{DetectScope, Detector, Found, Involved, Tension, TensionKind, TensionSeverity};
use crate::RdyError;
use crate::semantic::state::OpenTension;
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

/// The pair key of tension `t`: that of its two involved blocks and their contents, when it
/// has exactly two distinct blocks with contents (a semantic tension always has).
pub fn pair_of(t: &Tension) -> Option<Hash> {
    key_of(&t.involved)
}

/// The sides of `item` in the scope `cx` reads: in flight, every live write of the block that
/// writes the judged content, and canon when no in-flight write touches the block and canon
/// holds that content; in canon, canon only.
fn sides_of(cx: &DetectCx<'_>, item: &PairItem) -> Result<Vec<Involved>, RdyError> {
    let mut out = vec![];
    if cx.scope() == DetectScope::Inflight {
        for w in cx.writes.live_on(&item.block) {
            let i = cx.of_write(&w);
            if i.content == Some(item.content) {
                out.push(i);
            }
        }
        if !cx.writes.on(&item.block).is_empty() {
            return Ok(out);
        }
    }
    let canon = cx.canon_m()?;
    if let Some(b) = canon.get(&item.block)
        && cx.content_of(b) == item.content
        && let Some((side, v)) = cx.canon_side(&item.block)?
    {
        out.push(Involved {
            side,
            block: item.block.clone(),
            version: Some(v),
            content: Some(item.content),
        });
    }
    Ok(out)
}

impl Detector for Semantic {
    fn kind(&self) -> TensionKind {
        TensionKind::Semantic
    }

    fn stored(&self) -> bool {
        true
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
        // with seeds, the records that involve a seed (an index lookup each), else every record
        let open: Vec<Arc<OpenTension>> = match seeds {
            Some(seeds) => {
                let mut by_key: BTreeMap<Hash, Arc<OpenTension>> = BTreeMap::new();
                for s in seeds {
                    for t in state.involving(s) {
                        by_key.entry(t.record.key).or_insert(t);
                    }
                }
                by_key.into_values().collect()
            }
            None => state.tensions().cloned().collect(),
        };
        for t in open {
            let items = &t.record.items;
            let [a, b] = items.as_slice() else {
                continue;
            };
            let (lo, hi) = if a.block <= b.block {
                (&a.block, &b.block)
            } else {
                (&b.block, &a.block)
            };
            let summary = format!("a reviewer judged that {lo} and {hi} disagree");
            let (sa, sb) = (sides_of(cx, a)?, sides_of(cx, b)?);
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
