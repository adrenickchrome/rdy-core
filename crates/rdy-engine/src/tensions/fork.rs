//! Fork: two decisions supersede the same target (the same part, or the whole).
//!
//! In flight: a live in-flight decision write against another in-flight decision, or against a
//! canon decision with no in-flight write. In canon (FW-CH6): two live canon decisions that
//! supersede the same target. A fork of canon stays until one of its decisions stops
//! superseding the target, so a fork whose last in-flight side lands keeps its vkey.
use super::{DetectCx, DetectScope, Detector, Found, Involved, SideRef, TensionKind};
use crate::RdyError;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{Block, BlockId, Hash};
use std::collections::BTreeSet;

/// This kind's detector.
pub struct Fork;

/// One decision that supersedes the target: the side that holds it, its version and the part it
/// names (`None`: the whole).
struct Candidate {
    block: BlockId,
    version: Option<Hash>,
    side: SideRef,
    part: Option<String>,
}

impl Candidate {
    fn in_flight(&self) -> bool {
        matches!(self.side, SideRef::Change(_))
    }
}

/// The targets a decision `b` supersedes.
fn targets_of(b: &Block, into: &mut BTreeSet<BlockId>) {
    if b.block_type == decision::DECISION {
        for l in b.links.get(decision::SUPERSEDES).into_iter().flatten() {
            into.insert(l.target.clone());
        }
    }
}

/// The targets to examine: with `seeds`, the seeds as targets and the targets of the decisions
/// the seeds are (live in-flight writes, and canon's version); without, every target a live
/// in-flight write supersedes, and in the canon scope every target canon has a decision for.
fn targets(
    cx: &DetectCx<'_>,
    seeds: Option<&BTreeSet<BlockId>>,
) -> Result<BTreeSet<BlockId>, RdyError> {
    let mut out = BTreeSet::new();
    match seeds {
        Some(seeds) => {
            let canon = cx.canon_m()?;
            for s in seeds {
                out.insert(s.clone());
                for w in cx.writes.live_on(s) {
                    if let Some(b) = w.new.as_deref() {
                        targets_of(b, &mut out);
                    }
                }
                if let Some(b) = canon.get(s) {
                    targets_of(b, &mut out);
                }
            }
        }
        None => {
            for w in cx.writes.live() {
                if let Some(b) = w.new.as_deref() {
                    targets_of(b, &mut out);
                }
            }
            if cx.scope() == DetectScope::Canon {
                out.extend(cx.canon_index().supersedes.keys().cloned());
            }
        }
    }
    Ok(out)
}

/// The decisions that supersede `t`: live in-flight decision writes (not rejected), and canon's
/// live decisions that have no in-flight write at all.
fn candidates(cx: &DetectCx<'_>, t: &BlockId) -> Result<Vec<Candidate>, RdyError> {
    let mut out = vec![];
    for (w, part) in cx.writes.superseding(t) {
        let rejected = w
            .new
            .as_deref()
            .is_some_and(|b| decision::status(b) == Some(DecisionStatus::Rejected));
        if !rejected {
            out.push(Candidate {
                block: w.block.clone(),
                version: w.version,
                side: SideRef::Change(w.change),
                part,
            });
        }
    }
    for (d, part) in cx.canon_index().supersedes.get(t).into_iter().flatten() {
        if !cx.writes.all_on(d).is_empty() {
            continue;
        }
        if let Some((side, v)) = cx.canon_side(d)? {
            out.push(Candidate {
                block: d.clone(),
                version: Some(v),
                side,
                part: part.clone(),
            });
        }
    }
    Ok(out)
}

impl Detector for Fork {
    fn kind(&self) -> TensionKind {
        TensionKind::Fork
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        for t in targets(cx, seeds)? {
            let cands = candidates(cx, &t)?;
            for (i, x) in cands.iter().enumerate() {
                for y in &cands[i + 1..] {
                    let parts_meet = x.part.is_none() || y.part.is_none() || x.part == y.part;
                    let anchored =
                        cx.scope() == DetectScope::Canon || x.in_flight() || y.in_flight();
                    if x.block == y.block || !parts_meet || !anchored {
                        continue;
                    }
                    let (x, y) = if x.block <= y.block { (x, y) } else { (y, x) };
                    let part = match (&x.part, &y.part) {
                        (Some(p), Some(q)) if p == q => format!(" (part {p})"),
                        _ => String::new(),
                    };
                    out.push(Found {
                        kind: TensionKind::Fork,
                        involved: [x, y]
                            .into_iter()
                            .map(|c| Involved {
                                side: c.side.clone(),
                                block: c.block.clone(),
                                version: c.version,
                            })
                            .collect(),
                        summary: format!("{} and {} both supersede {t}{part}", x.block, y.block),
                        stable: true,
                    });
                }
            }
        }
        Ok(())
    }
}
