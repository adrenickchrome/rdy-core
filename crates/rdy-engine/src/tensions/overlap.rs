//! Area overlap: two in-flight decisions affect one area and touch the same blocks; at read
//! time, an open intent covers a block another view's change edits.
//!
//! The event-derived part has no canon form (FW-CH6): its trigger is two decisions in flight.
//! When one of them lands, only one is in flight, so the tension resolves.
use super::index::Write;
use super::{
    DetectCx, DetectScope, Detector, Found, Involved, SideRef, Tension, TensionKind, tension_from,
};
use crate::intent::intent_label;
use crate::{RdyError, ReadState, Scope};
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// This kind's detector.
pub struct AreaOverlap;

/// One live, non-rejected decision write, with the areas it is in and the blocks its change
/// touches (the blocks the change writes and the decision's `affects` targets).
struct Candidate {
    write: Arc<Write>,
    touched: BTreeSet<BlockId>,
}

/// The candidates, and for each area the indexes of those in it.
type ByArea = BTreeMap<String, Vec<usize>>;

fn candidates(cx: &DetectCx<'_>) -> Result<(Vec<Candidate>, ByArea), RdyError> {
    let mut out: Vec<Candidate> = vec![];
    let mut by_area = ByArea::new();
    let mut inflight = None;
    for w in cx.writes.live() {
        let Some(b) = w.new.as_deref() else {
            continue;
        };
        if b.block_type != decision::DECISION
            || decision::status(b) == Some(DecisionStatus::Rejected)
        {
            continue;
        }
        let m = match &inflight {
            Some(m) => Arc::clone(m),
            None => {
                let m = cx.inflight_m()?;
                inflight = Some(Arc::clone(&m));
                m
            }
        };
        let areas = cx.st.areas_in(&m, b);
        if areas.is_empty() {
            continue;
        }
        let mut touched: BTreeSet<BlockId> = cx
            .writes
            .change(&w.change)
            .map(|c| c.blocks.iter().cloned().collect())
            .unwrap_or_default();
        touched.extend(
            b.links
                .get(decision::AFFECTS)
                .into_iter()
                .flatten()
                .map(|l| l.target.clone()),
        );
        for a in areas {
            by_area.entry(a).or_default().push(out.len());
        }
        out.push(Candidate {
            write: Arc::clone(w),
            touched,
        });
    }
    Ok((out, by_area))
}

impl Detector for AreaOverlap {
    fn kind(&self) -> TensionKind {
        TensionKind::AreaOverlap
    }

    fn detect(
        &self,
        cx: &DetectCx<'_>,
        seeds: Option<&BTreeSet<BlockId>>,
        out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        if cx.scope() == DetectScope::Canon || cx.st.area_matcher().is_empty() {
            return Ok(());
        }
        let (cands, by_area) = candidates(cx)?;
        let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (area, members) in &by_area {
            for (n, &i) in members.iter().enumerate() {
                for &j in &members[n + 1..] {
                    let (x, y) = (&cands[i], &cands[j]);
                    let (xw, yw) = (&x.write, &y.write);
                    if xw.change == yw.change
                        || cx.writes.common_view(&xw.change, &yw.change)
                        || seeds.is_some_and(|s| !s.contains(&xw.block) && !s.contains(&yw.block))
                    {
                        continue;
                    }
                    let Some(shared) = x.touched.intersection(&y.touched).next() else {
                        continue;
                    };
                    if !seen.insert((i, j)) {
                        continue;
                    }
                    let (xw, yw) = if (&xw.block, xw.change) <= (&yw.block, yw.change) {
                        (xw, yw)
                    } else {
                        (yw, xw)
                    };
                    out.push(Found {
                        kind: TensionKind::AreaOverlap,
                        involved: [xw, yw].into_iter().map(|w| cx.of_write(w)).collect(),
                        summary: format!(
                            "{} and {} both affect area {area} and touch {shared}",
                            xw.block, yw.block
                        ),
                        stable: true,
                    });
                }
            }
        }
        Ok(())
    }
}

impl ReadState {
    /// The read-time area overlaps at `now_ms`: each open, unexpired intent against the live
    /// writes of other views' changes on the blocks it covers. Never in the event-derived set.
    /// The in-flight writes are read once, and each written block's areas once.
    pub fn intent_tensions(&self, now_ms: i64) -> Result<Vec<Tension>, RdyError> {
        let open: Vec<_> = self
            .coord_heads()
            .open_intents
            .iter()
            .filter(|(_, o)| now_ms < o.intent.expires_at_ms)
            .collect();
        if open.is_empty() {
            return Ok(vec![]);
        }
        let any_area = open.iter().any(|(_, o)| o.intent.area.is_some());
        let set = self.tensions()?;
        let writes = set.writes();
        let tracking = self.reader().tracking();
        let mut out: BTreeMap<Hash, Tension> = BTreeMap::new();
        // live writes come by block: the areas of the block in hand
        let mut areas: Option<(BlockId, Vec<String>)> = None;
        for w in writes.live() {
            if any_area && areas.as_ref().is_none_or(|(b, _)| *b != w.block) {
                areas = Some((
                    w.block.clone(),
                    self.block_areas(&Scope::Inflight, &w.block)?,
                ));
            }
            let in_areas = |a: &String| areas.as_ref().is_some_and(|(_, v)| v.contains(a));
            let views = writes
                .change(&w.change)
                .map(|c| c.views.as_slice())
                .unwrap_or(&[]);
            let mut content = None;
            for (stable, o) in &open {
                let it = &o.intent;
                let covers = it.blocks.contains(&w.block) || it.area.as_ref().is_some_and(in_areas);
                if !covers || views.contains(&it.view) {
                    continue;
                }
                let content = *content
                    .get_or_insert_with(|| w.new.as_deref().map(|b| b.content_hash_with(tracking)));
                let involved = vec![
                    Involved {
                        side: SideRef::Intent {
                            stable: **stable,
                            version: o.version,
                        },
                        block: w.block.clone(),
                        version: Some(o.version),
                        content: Some(o.version),
                    },
                    Involved {
                        side: SideRef::Change(w.change),
                        block: w.block.clone(),
                        version: w.version,
                        content,
                    },
                ];
                let found = Found {
                    kind: TensionKind::AreaOverlap,
                    summary: format!(
                        "intent {} ({}) covers {}, which change {} edits",
                        intent_label(stable),
                        it.actor,
                        w.block,
                        w.change.short()
                    ),
                    involved,
                    stable: true,
                };
                if let Some(t) = tension_from(self.pack(), None, found, true, DetectScope::Inflight)
                {
                    out.insert(t.id, t);
                }
            }
        }
        Ok(out.into_values().collect())
    }
}
