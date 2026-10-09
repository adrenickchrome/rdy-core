//! Deprecated reference: content references or links to deprecated content.
//!
//! In flight: a live write adds a reference or an impact link to deprecated content. In canon
//! (FW-CH6a): live canon content that references deprecated canon content is a standing
//! condition, so a reference that lands keeps its vkey (it is re-keyed to canon). Deprecated
//! content that references deprecated content raises nothing in canon.
use super::{DetectCx, DetectScope, Detector, Found, Involved, TensionKind, pointing_edge};
use crate::RdyError;
use rdy_graph::{Edge, edges_of};
use rdy_model::{Block, BlockId, Hash};
use std::collections::BTreeSet;
use std::sync::Arc;

/// This kind's detector.
pub struct DeprecatedReference;

/// Whether `base` has an edge of the same kind to the same target as `e`.
fn had_edge(base: Option<&Block>, e: &Edge) -> bool {
    base.is_some_and(|b| edges_of(b).iter().any(|o| o.kind == e.kind && o.to == e.to))
}

fn found(y: Involved, x: Involved, who: &str) -> Found {
    let summary = format!("{} ({who}) references deprecated {}", y.block, x.block);
    Found {
        kind: TensionKind::DeprecatedReference,
        involved: vec![y, x],
        summary,
        stable: true,
    }
}

impl DeprecatedReference {
    /// The in-flight form: the live writes (those of `seeds` as citers, and those that point at a
    /// seed) with a new pointing edge to deprecated content.
    fn inflight(
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let mut cands: Vec<(Arc<crate::tensions::Write>, Edge)> = vec![];
        match seeds {
            None => {
                for w in cx.writes.live() {
                    if let Some(b) = w.new.as_deref() {
                        cands.extend(edges_of(b).into_iter().map(|e| (Arc::clone(w), e)));
                    }
                }
            }
            Some(seeds) => {
                for s in seeds {
                    for w in cx.writes.live_on(s) {
                        if let Some(b) = w.new.as_deref() {
                            cands.extend(edges_of(b).into_iter().map(|e| (Arc::clone(&w), e)));
                        }
                    }
                    cands.extend(cx.writes.pointing_at(s));
                }
            }
        }
        let mut seen: BTreeSet<(Hash, BlockId, BlockId)> = BTreeSet::new();
        for (w, e) in cands {
            if !pointing_edge(&e) || e.to == w.block {
                continue;
            }
            let key = (w.change, w.block.clone(), e.to.clone());
            if seen.contains(&key) {
                continue;
            }
            let base = match w.base {
                Some(v) => Some(cx.st.reader().get_block(&v)?),
                None => None,
            };
            if had_edge(base.as_ref(), &e) || !cx.deprecated(&e.to)? {
                continue;
            }
            let Some((side, version)) = cx.side_at(&e.to)? else {
                continue;
            };
            // mark the pair only once a tension is raised: an old edge of another kind to the
            // same target must not hide a new one
            seen.insert(key);
            out.push(found(
                cx.of_write(&w),
                cx.involved(side, &e.to, version)?,
                &format!("change {}", w.change.short()),
            ));
        }
        Ok(())
    }

    /// The canon form: live canon blocks with a pointing edge to a deprecated canon block. The
    /// canon part is always a full run, so it reads every canon block.
    fn canon(cx: &DetectCx<'_>, out: &mut Vec<Found>) -> Result<(), RdyError> {
        let m = cx.canon_m()?;
        let ys: Vec<BlockId> = m.iter().map(|b| b.id.clone()).collect();
        let mut seen: BTreeSet<(BlockId, BlockId)> = BTreeSet::new();
        for y in ys {
            let Some(b) = m.get(&y) else {
                continue;
            };
            let mut live = None;
            for e in edges_of(b) {
                if !pointing_edge(&e) || e.to == y || seen.contains(&(y.clone(), e.to.clone())) {
                    continue;
                }
                if !cx.deprecated(&e.to)? {
                    continue;
                }
                if *live.get_or_insert(!cx.deprecated(&y)?) {
                    seen.insert((y.clone(), e.to.clone()));
                    if let (Some((ys, yv)), Some((xs, xv))) =
                        (cx.canon_side(&y)?, cx.canon_side(&e.to)?)
                    {
                        out.push(found(
                            cx.involved(ys, &y, Some(yv))?,
                            cx.involved(xs, &e.to, Some(xv))?,
                            "canon",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Detector for DeprecatedReference {
    fn kind(&self) -> TensionKind {
        TensionKind::DeprecatedReference
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        match cx.scope() {
            DetectScope::Inflight => Self::inflight(cx, seeds, out),
            DetectScope::Canon => Self::canon(cx, out),
        }
    }
}
