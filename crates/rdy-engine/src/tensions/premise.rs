//! Premise shift: an in-flight change alters or retires a block that another change's content
//! cites with a pin.
//!
//! A *changer* of `X` is a live write on `X` that updates or deletes it (involved: `X` at the
//! write's version), or a live decision write whose supersedes or revokes link to `X` is new in
//! flight (involved: the decision at its version). A decision that canon already shows
//! superseding or revoking `X` is not a changer of `X`: an edit of it (a tracking edit, say)
//! changes nothing about `X`, and a premise superseded in canon is a deprecated reference. A
//! *citer* is a live write on `Y` whose block has a premise edge ([`premise_edge`]) to `X` with a
//! pin; a decision's reference to a block it supersedes or revokes is not a citation (it names
//! what it retires; the graph rules the same for suspects). A changer and a citer from two
//! changes that share no view make a premise shift when the
//! changer deletes, supersedes or revokes `X`, or writes content other than the pin. The canon
//! form is a citer whose pin is neither canon's content of `X` nor the content of an in-flight
//! write of `X` by the citer's change or a change that shares a view with it (involved: canon's
//! side of `X`, and `Y`). A citer can pin only content its own view sees, so a write in an
//! unrelated view never hides a stale pin.
//!
//! No all-canon form (FW-CH6a): in the canon scope this detector reports nothing. A canon block
//! with a stale pin is a suspect, and the suspect covers it.
use super::index::retired_by;
use super::{
    DetectCx, DetectScope, Detector, Found, Involved, SideRef, TensionKind, Write, Writes,
    premise_edge,
};
use crate::{Materialized, RdyError};
use rdy_graph::{Edge, EdgeKind, edges_of};
use rdy_model::{Block, BlockId, Hash, Tracking};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// This kind's detector.
pub struct PremiseShift;

/// One pinned citation: the citing write, the cited block and the pin.
type Citation = (Arc<Write>, BlockId, Hash);

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
        let canon_m = cx.canon_m()?;
        // pins are content hashes without the tracking fields (trk1)
        let mut content = Contents::new(cx.st.reader().tracking());
        for (w, x, pin) in citations {
            let mut shifted = vec![];
            for (a, retires) in changers(writes, &canon_m, &x) {
                if a.change == w.change || writes.common_view(&a.change, &w.change) {
                    continue;
                }
                let differs = retires || content.of_write(&a) != Some(pin);
                if differs {
                    shifted.push(cx.of_write(&a));
                }
            }
            let canon_content = match (canon_m.version_of(&x), canon_m.get(&x)) {
                (Some(v), Some(b)) => Some(content.of(v, b)),
                _ => None,
            };
            if canon_content != Some(pin)
                && !kept_deleted_citation(writes, &canon_m, &w, &x)
                && !in_flight_content(writes, &w, &x, pin, &mut content)
            {
                let (side, version) = match cx.canon_side(&x)? {
                    Some((side, v)) => (side, Some(v)),
                    None => (SideRef::Canon { landing: None }, None),
                };
                shifted.push(Involved {
                    side,
                    block: x.clone(),
                    version,
                    content: canon_content,
                });
            }
            for a in shifted {
                let label = a.side.short();
                let involved = vec![a, cx.of_write(&w)];
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

/// Content hashes (without the tracking fields) memoised by version: a hub that many in-flight
/// changes cite is hashed once per detection, not once per citation.
struct Contents<'t> {
    tracking: &'t Tracking,
    by_version: HashMap<Hash, Hash>,
}

impl<'t> Contents<'t> {
    fn new(tracking: &'t Tracking) -> Contents<'t> {
        Contents {
            tracking,
            by_version: HashMap::new(),
        }
    }

    /// The content of block `b`, whose version is `v`.
    fn of(&mut self, v: Hash, b: &Block) -> Hash {
        *self
            .by_version
            .entry(v)
            .or_insert_with(|| b.content_hash_with(self.tracking))
    }

    /// The content write `w` writes (`None` for a delete).
    fn of_write(&mut self, w: &Write) -> Option<Hash> {
        match (w.version, w.new.as_deref()) {
            (Some(v), Some(b)) => Some(self.of(v, b)),
            _ => None,
        }
    }
}

/// The pinned premise citations to examine, each once, sorted by citing block, change and cited
/// block (so a carry and a rebuild visit them in one order, and a tension two citations give
/// keeps one summary). With no seeds, every live write's. With seeds: a seed's live writes as
/// citers, the citers of a seed as a changed block, and the citers of the blocks that a seed's
/// decision supersedes or revokes. A decision's reference to a block it supersedes or revokes
/// is not a citation (the graph rules the same for suspects): it names what it retires.
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
                    if let Some(b) = w.new.as_deref() {
                        targets.extend(retired_by(b));
                    }
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
        if e.kind == EdgeKind::References
            && w.new
                .as_deref()
                .is_some_and(|b| retired_by(b).contains(&e.to))
        {
            continue;
        }
        if let Some(pin) = e.pin
            && seen.insert((w.change, w.block.clone(), e.to.clone()))
        {
            out.push((w, e.to, pin));
        }
    }
    out.sort_by(|(w1, x1, _), (w2, x2, _)| {
        (&w1.block, w1.change, x1).cmp(&(&w2.block, w2.change, x2))
    });
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

/// The changers of `x`: its live updates and deletes (`false`), and the live decision writes
/// that supersede or revoke it (`true`: a shift whatever the pin), less those whose canon block
/// already supersedes or revokes `x` (their link to `x` is not new in flight).
fn changers(writes: &Writes, canon_m: &Materialized, x: &BlockId) -> Vec<(Arc<Write>, bool)> {
    let mut out: Vec<(Arc<Write>, bool)> = writes
        .live_on(x)
        .into_iter()
        .filter(|w| w.base.is_some())
        .map(|w| (w, false))
        .collect();
    out.extend(
        writes
            .retiring(x)
            .into_iter()
            .filter(|d| {
                !canon_m
                    .get(&d.block)
                    .is_some_and(|b| retired_by(b).contains(x))
            })
            .map(|d| (d, true)),
    );
    out
}

/// p-QMGB4 (task-5 R1): whether citer `w` keeps its citation of `x`, which `w`'s view does not
/// hold: its block records `deleted` for `x` ([`rdy_graph::keeps_deleted_citation`]), `x` is
/// absent there (canon no longer holds it, or a write that `w` can see deletes it, as when the
/// view deletes a premise that changed in canon after the pin), and no in-flight write that `w`
/// can see brings `x` back. An unacknowledged deletion and a visible restoration keep the check.
/// The graph's suspects rule the same.
fn kept_deleted_citation(writes: &Writes, canon_m: &Materialized, w: &Write, x: &BlockId) -> bool {
    if !w
        .new
        .as_deref()
        .is_some_and(|b| rdy_graph::keeps_deleted_citation(b, x))
    {
        return false;
    }
    let seen: Vec<Arc<Write>> = writes
        .all_on(x)
        .into_iter()
        .filter(|w2| w2.change == w.change || writes.common_view(&w2.change, &w.change))
        .collect();
    let deleted_here = seen.iter().any(|w2| w2.new.is_none());
    (canon_m.get(x).is_none() || deleted_here) && !seen.iter().any(|w2| w2.new.is_some())
}

/// Whether an in-flight write of `x` (live or not) that citer `w` can see has content `pin`: a
/// write by `w`'s change, or by a change that shares a view with it.
fn in_flight_content(
    writes: &Writes,
    w: &Write,
    x: &BlockId,
    pin: Hash,
    content: &mut Contents<'_>,
) -> bool {
    writes.all_on(x).iter().any(|w2| {
        (w2.change == w.change || writes.common_view(&w2.change, &w.change))
            && content.of_write(w2) == Some(pin)
    })
}
