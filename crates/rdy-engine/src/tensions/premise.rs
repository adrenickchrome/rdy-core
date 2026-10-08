//! Premise shift: an in-flight change alters or retires a block that another change's content
//! cites with a pin.
//!
//! A *changer* of `X` is a live write on `X` that updates or deletes it (involved: `X` at the
//! write's version), or a live decision write that supersedes or revokes `X` (involved: the
//! decision at its version). A *citer* is a live write on `Y` whose block has a premise edge
//! ([`premise_edge`]) to `X` with a pin. A changer and a citer from two changes that share no
//! view make a premise shift when the changer deletes, supersedes or revokes `X`, or writes
//! content other than the pin. The canon form is a citer whose pin is neither canon's content of
//! `X` nor the content of any in-flight write of `X` (involved: canon's side of `X`, and `Y`).
//!
//! No all-canon form (FW-CH6a): in the canon scope this detector reports nothing. A canon block
//! with a stale pin is a suspect, and the suspect covers it.
use super::{
    DetectCx, DetectScope, Detector, Found, Involved, SideRef, TensionKind, Write, Writes,
    premise_edge,
};
use crate::RdyError;
use rdy_graph::{Edge, edges_of};
use rdy_model::{BlockId, Hash, Tracking, decision};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// This kind's detector.
pub struct PremiseShift;

/// One pinned citation: the citing write, the cited block and the pin.
type Citation = (Arc<Write>, BlockId, Hash);

/// The live decision writes that supersede or revoke a block, by that block.
type Retiring = BTreeMap<BlockId, Vec<Arc<Write>>>;

impl Detector for PremiseShift {
    fn kind(&self) -> TensionKind {
        TensionKind::PremiseShift
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() == DetectScope::Canon {
            return Ok(());
        }
        let writes = cx.writes;
        let citations = citations(writes, seeds);
        if citations.is_empty() {
            return Ok(());
        }
        let retiring = retiring(writes);
        let canon_m = cx.canon_m()?;
        // pins are content hashes without the tracking fields (trk1)
        let tracking = Arc::clone(cx.canon_graph()?.tracking());
        for (w, x, pin) in citations {
            let mut shifted = vec![];
            for (a, retires) in changers(writes, &retiring, &x) {
                if a.change == w.change || writes.common_view(&a.change, &w.change) {
                    continue;
                }
                let differs = retires
                    || a.new
                        .as_deref()
                        .is_none_or(|b| b.content_hash_with(&tracking) != pin);
                if differs {
                    shifted.push(Involved {
                        side: SideRef::Change(a.change),
                        block: a.block.clone(),
                        version: a.version,
                    });
                }
            }
            let canon_content = canon_m.get(&x).map(|b| b.content_hash_with(&tracking));
            if canon_content != Some(pin) && !in_flight_content(writes, &x, pin, &tracking) {
                let (side, version) = match cx.canon_side(&x)? {
                    Some((side, v)) => (side, Some(v)),
                    None => (SideRef::Canon { landing: None }, None),
                };
                shifted.push(Involved {
                    side,
                    block: x.clone(),
                    version,
                });
            }
            for a in shifted {
                let label = a.side.short();
                let involved = vec![
                    a,
                    Involved {
                        side: SideRef::Change(w.change),
                        block: w.block.clone(),
                        version: w.version,
                    },
                ];
                if seeds.is_some_and(|s| !involved.iter().any(|i| s.contains(&i.block))) {
                    continue;
                }
                out.push(Found {
                    kind: TensionKind::PremiseShift,
                    summary: format!(
                        "{x} changed in {label}; {} (change {}) still cites it at the old version",
                        w.block,
                        w.change.short()
                    ),
                    stable: cx.stable(&x)? || cx.stable(&w.block)?,
                    involved,
                });
            }
        }
        Ok(())
    }
}

/// The pinned premise citations to examine, each once. With no seeds, every live write's. With
/// seeds: a seed's live writes as citers, the citers of a seed as a changed block, and the
/// citers of the blocks that a seed's decision supersedes or revokes.
fn citations(writes: &Writes, seeds: Option<&BTreeSet<BlockId>>) -> Vec<Citation> {
    // (citing write, its edge)
    let mut edges: Vec<(Arc<Write>, Edge)> = vec![];
    match seeds {
        None => {
            for w in writes.live() {
                edges.extend(own_edges(w));
            }
        }
        Some(seeds) => {
            let mut targets: BTreeSet<BlockId> = seeds.clone();
            for s in seeds {
                for w in writes.live_on(s) {
                    edges.extend(own_edges(&w));
                    targets.extend(retired_by(&w));
                }
            }
            for x in &targets {
                edges.extend(writes.pointing_at(x));
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for (w, e) in edges {
        if !premise_edge(&e) {
            continue;
        }
        if let Some(pin) = e.pin
            && seen.insert((w.change, w.block.clone(), e.to.clone()))
        {
            out.push((w, e.to, pin));
        }
    }
    out
}

/// The edges of the block that write `w` writes, each with `w`.
fn own_edges(w: &Arc<Write>) -> Vec<(Arc<Write>, Edge)> {
    w.new
        .as_deref()
        .map(edges_of)
        .unwrap_or_default()
        .into_iter()
        .map(|e| (Arc::clone(w), e))
        .collect()
}

/// The blocks that write `w`'s decision supersedes or revokes (none for another block type).
fn retired_by(w: &Write) -> Vec<BlockId> {
    let Some(b) = w
        .new
        .as_deref()
        .filter(|b| b.block_type == decision::DECISION)
    else {
        return vec![];
    };
    [decision::SUPERSEDES, decision::REVOKES]
        .into_iter()
        .flat_map(|k| b.links.get(k).into_iter().flatten())
        .map(|l| l.target.clone())
        .collect()
}

/// The live decision writes that supersede or revoke each block, each once per block.
fn retiring(writes: &Writes) -> Retiring {
    let mut by_target = Retiring::new();
    for w in writes.live() {
        for t in retired_by(w) {
            let list = by_target.entry(t).or_default();
            if !list.iter().any(|d| Arc::ptr_eq(d, w)) {
                list.push(Arc::clone(w));
            }
        }
    }
    by_target
}

/// The changers of `x`: its live updates and deletes (`false`), and the live decision writes
/// that supersede or revoke it (`true`: a shift whatever the pin).
fn changers(writes: &Writes, retiring: &Retiring, x: &BlockId) -> Vec<(Arc<Write>, bool)> {
    let mut out: Vec<(Arc<Write>, bool)> = writes
        .live_on(x)
        .into_iter()
        .filter(|w| w.base.is_some())
        .map(|w| (w, false))
        .collect();
    out.extend(
        retiring
            .get(x)
            .into_iter()
            .flatten()
            .map(|d| (Arc::clone(d), true)),
    );
    out
}

/// Whether an in-flight write of `x` (live or not) has content `pin`.
fn in_flight_content(writes: &Writes, x: &BlockId, pin: Hash, tracking: &Tracking) -> bool {
    writes
        .all_on(x)
        .iter()
        .any(|w| w.new.as_deref().map(|b| b.content_hash_with(tracking)) == Some(pin))
}
