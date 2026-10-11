//! Deprecated reference: content references or links to deprecated content, or names it in plain
//! text.
//!
//! In flight: a live write adds a reference or an impact link to deprecated content. In canon
//! (FW-CH6a): live canon content that references deprecated canon content is a standing
//! condition, so a reference that lands keeps its vkey (it is re-keyed to canon). Deprecated
//! content that references deprecated content raises nothing in canon.
//!
//! The mention form (FW-CH2): a block that names a deprecated definition by its name or an alias
//! in its text, without a tie to it, is the same tension as a reference to it. In flight, the
//! write must be the first to name it; in canon, the naming block must be live. A block that
//! both references and names a definition gets the edge form only.
use super::{DetectCx, DetectScope, Detector, Found, Involved, TensionKind, pointing_edge};
use crate::RdyError;
use crate::lexicon::tied;
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

/// The summary of the mention form: `y` names the deprecated definition `x` in its text.
fn found_mention(y: Involved, x: Involved, who: &str, name: &str) -> Found {
    let summary = format!(
        "{} ({who}) names deprecated {} ({name}) in its text",
        y.block, x.block
    );
    Found {
        kind: TensionKind::DeprecatedReference,
        involved: vec![y, x],
        summary,
        stable: true,
    }
}

/// Whether `b` has a pointing edge to `x`: the edge form reports that pair.
fn points_at(b: &Block, x: &BlockId) -> bool {
    edges_of(b).iter().any(|e| pointing_edge(e) && e.to == *x)
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

impl DeprecatedReference {
    /// The in-flight mention form: live writes whose new block names a deprecated definition in
    /// plain text, and whose base did not. A run around `seeds` reads the writes on the seeds,
    /// and every live write when a seed is a definition (its deprecation can change what the
    /// other writes name).
    fn inflight_mentions(
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        let m = cx.inflight_m()?;
        let writes: Vec<Arc<crate::tensions::Write>> = match seeds {
            Some(seeds)
                if !seeds
                    .iter()
                    .any(|s| m.get(s).is_some_and(|b| cx.defines(b))) =>
            {
                seeds.iter().flat_map(|s| cx.writes.live_on(s)).collect()
            }
            _ => cx.writes.live().map(Arc::clone).collect(),
        };
        let lexicon = cx.lexicon()?;
        let g = cx.inflight_graph()?;
        let mut seen: BTreeSet<(Hash, BlockId, BlockId)> = BTreeSet::new();
        for w in writes {
            let Some(b) = w.new.as_deref() else {
                continue;
            };
            let mut base_names: Option<Vec<String>> = None;
            for mention in lexicon.mentions(b) {
                let Ok(xid) = BlockId::parse(&mention.entity) else {
                    continue;
                };
                let Some(xb) = m.get(&xid) else {
                    continue;
                };
                if !cx.defines(xb)
                    || tied(&g, &w.block, &xid)
                    || points_at(b, &xid)
                    || !cx.deprecated(&xid)?
                {
                    continue;
                }
                let key = (w.change, w.block.clone(), xid.clone());
                if seen.contains(&key) {
                    continue;
                }
                if base_names.is_none() {
                    base_names = Some(match w.base {
                        Some(v) => lexicon
                            .mentions(&cx.st.reader().get_block(&v)?)
                            .into_iter()
                            .map(|x| x.entity)
                            .collect(),
                        None => vec![],
                    });
                }
                if base_names.iter().flatten().any(|e| *e == mention.entity) {
                    continue;
                }
                let Some((side, version)) = cx.side_at(&xid)? else {
                    continue;
                };
                seen.insert(key);
                out.push(found_mention(
                    cx.of_write(&w),
                    cx.involved(side, &xid, version)?,
                    &format!("change {}", w.change.short()),
                    xb.field_str("name").unwrap_or_default(),
                ));
            }
        }
        Ok(())
    }

    /// The canon mention form: live canon blocks that name a deprecated canon definition in
    /// plain text without a tie to it. Nothing is read when canon has no deprecated definition.
    fn canon_mentions(cx: &DetectCx<'_>, out: &mut Vec<Found>) -> Result<(), RdyError> {
        let m = cx.canon_m()?;
        let mut retired: BTreeSet<BlockId> = BTreeSet::new();
        for b in m.iter() {
            if cx.defines(b) && cx.deprecated(&b.id)? {
                retired.insert(b.id.clone());
            }
        }
        if retired.is_empty() {
            return Ok(());
        }
        let lexicon = cx.lexicon()?;
        let g = cx.canon_graph()?;
        for b in m.iter() {
            let mut live = None;
            for mention in lexicon.mentions(b) {
                let Ok(xid) = BlockId::parse(&mention.entity) else {
                    continue;
                };
                if !retired.contains(&xid) || tied(&g, &b.id, &xid) || points_at(b, &xid) {
                    continue;
                }
                if !*live.get_or_insert(!cx.deprecated(&b.id)?) {
                    break;
                }
                if let (Some((ys, yv)), Some((xs, xv))) =
                    (cx.canon_side(&b.id)?, cx.canon_side(&xid)?)
                {
                    let name = m
                        .get(&xid)
                        .and_then(|x| x.field_str("name"))
                        .unwrap_or_default();
                    out.push(found_mention(
                        cx.involved(ys, &b.id, Some(yv))?,
                        cx.involved(xs, &xid, Some(xv))?,
                        "canon",
                        name,
                    ));
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
            DetectScope::Inflight => {
                Self::inflight(cx, seeds, out)?;
                Self::inflight_mentions(cx, seeds, out)
            }
            DetectScope::Canon => {
                Self::canon(cx, out)?;
                Self::canon_mentions(cx, out)
            }
        }
    }
}
