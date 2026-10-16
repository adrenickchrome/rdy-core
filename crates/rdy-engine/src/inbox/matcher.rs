//! The closed rules that put a log event in a session's inbox (plan M2e2, ledger F4; D-TPZ0W,
//! D-V4NZX, D-Z0EMP). The logged tension and suspect moves are read apart (`inbox::moves`); only
//! the events below match here.
use super::Entry;
use super::inflight::RunInFlight;
use crate::gate::{ProposalRef, version_number};
use crate::semantic::candidates::Request;
use crate::{E_UNKNOWN_BLOCK, Event, EventKind, RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash, Op};
use rdy_store::inbox::InboxSub;
use std::collections::BTreeSet;

/// The areas that concern the session: its declared areas, or else the areas of the blocks its
/// in-flight changes write (D-V4NZX).
pub fn effective_areas(sub: &InboxSub, infl: &RunInFlight) -> Vec<String> {
    if sub.areas.is_empty() {
        infl.areas.clone()
    } else {
        sub.areas.clone()
    }
}

/// The raw items of one batch (`batch`: its events, all of one `seq`, logged moves included, in
/// frame order) from raw index `skip` on, each with its index in the batch and the entry
/// `match_event` gives it. The indexes are fixed by the log, so a cursor's `skip` counts raw
/// items walked, matched or not (ruling task-4.1): a cut stays exact whatever the state does
/// later. Which items match is read from the state at read time. The walk is lazy.
pub fn batch_items<'a>(
    st: &'a ReadState,
    sub: &'a InboxSub,
    infl: &'a RunInFlight,
    batch: &'a [Event],
    skip: usize,
) -> impl Iterator<Item = Result<(usize, Option<Entry>), RdyError>> + 'a {
    batch
        .iter()
        .enumerate()
        .skip(skip)
        .map(move |(i, ev)| Ok((i, match_event(st, sub, infl, ev)?)))
}

/// The entry `ev` gives the session, under the closed rules of plan M2e2 (ledger F4); None for every other event.
pub fn match_event(
    st: &ReadState,
    sub: &InboxSub,
    infl: &RunInFlight,
    ev: &Event,
) -> Result<Option<Entry>, RdyError> {
    let entry = |event: &'static str, ids: Vec<String>, line: String| {
        Some(Entry {
            event,
            ids,
            line,
            seq: ev.seq,
        })
    };
    Ok(match &ev.kind {
        EventKind::Landed {
            landing,
            proposal,
            view,
        } => {
            if infl.views.contains(view) {
                return Ok(None);
            }
            let areas = effective_areas(sub, infl);
            if areas.is_empty() {
                return Ok(None);
            }
            let (n, hit) = landed_in_areas(st, landing, &areas)?;
            if n == 0 {
                return Ok(None);
            }
            let hit: Vec<String> = hit.into_iter().collect();
            entry(
                "landed",
                vec![landing.to_string(), proposal.to_string()],
                format!(
                    "landing {} of view {view} changed {n} blocks in your areas ({})",
                    landing.short(),
                    hit.join(", ")
                ),
            )
        }
        EventKind::ViewRebased {
            view,
            landing,
            conflicts,
        } => {
            if !infl.views.contains(view) || conflicts.is_empty() {
                return Ok(None);
            }
            let mut ids = vec![landing.to_string()];
            ids.extend(conflicts.iter().map(Hash::to_string));
            entry(
                "view_rebased",
                ids,
                format!(
                    "your view {view} rebased onto landing {} with {} conflicts",
                    landing.short(),
                    conflicts.len()
                ),
            )
        }
        EventKind::ViewClosed { view, reason, .. } => {
            if !infl.views.contains(view) {
                return Ok(None);
            }
            entry(
                "view_closed",
                vec![view.clone()],
                format!("your view {view} closed: {reason}"),
            )
        }
        EventKind::AltSetChanged {
            set,
            version,
            change,
            members,
        } => {
            let Some(v) = members.iter().find(|m| infl.views.contains(*m)) else {
                return Ok(None);
            };
            entry(
                "alt_set_changed",
                vec![set.to_string(), version.to_string()],
                format!(
                    "alternative set {} {change} (your view {v} is a member)",
                    set.short()
                ),
            )
        }
        EventKind::IntentPosted {
            intent,
            actor,
            run,
            area,
            blocks,
            ..
        } => {
            if *actor == sub.actor && *run == sub.run {
                return Ok(None);
            }
            let n = blocks
                .iter()
                .filter(|b| BlockId::parse(b).is_ok_and(|id| infl.blocks.contains(&id)))
                .count();
            let line = if n > 0 {
                format!(
                    "{actor} posted intent {} on {n} blocks you are changing",
                    intent.short()
                )
            } else if let Some(a) = area
                && effective_areas(sub, infl).contains(a)
            {
                format!("{actor} posted intent {} on area {a}", intent.short())
            } else {
                return Ok(None);
            };
            entry("intent_posted", vec![intent.to_string()], line)
        }
        EventKind::ChangeCommitted { change, actor, .. } => {
            if sub.role != "reviewer" || *actor == sub.actor {
                return Ok(None);
            }
            let mut n = 0usize;
            for q in st.requests_from(change)? {
                if counts_for(st, &sub.actor, &q) {
                    n += 1;
                }
            }
            if n == 0 {
                return Ok(None);
            }
            entry(
                "review_requested",
                vec![change.to_string()],
                format!(
                    "{n} review requests from change {}: call rdy_review_next",
                    change.short()
                ),
            )
        }
        // O10 (D-JV6Z8): the two events that put something in the needs-you queue. Neither reads
        // readiness, so a re-derived inbox stays the same.
        EventKind::Proposed { proposal, .. } => {
            let stable = st.reader().proposal(proposal)?.stable.unwrap_or(*proposal);
            let pref = ProposalRef {
                version: *proposal,
                stable,
                n: version_number(st, proposal)?,
            };
            entry(
                "pending_added",
                vec![proposal.to_string()],
                format!("{pref} waits for an answer; read rdy_pending"),
            )
        }
        EventKind::ReviewSubmitted { review, pair, .. } => {
            // a verdict that carries advice adds one entry that points at the review (ruling
            // task-11, rule 2); the queue says which proposal waits on it. The review id never
            // changes, so a re-derived inbox is the same.
            if !verdict_carries_advice(st, review, pair)? {
                return Ok(None);
            }
            entry(
                "pending_added",
                vec![review.to_string()],
                "new advice waits for an answer; read rdy_pending".into(),
            )
        }
        _ => None,
    })
}

/// Whether the verdict `review` carries the reviewer's advice: the first version of the pair's
/// tension is the one that review opened, and it holds the advice. `false` for any other verdict.
/// An acceptance adds a later version; it never changes the first one.
fn verdict_carries_advice(st: &ReadState, review: &Hash, pair: &Hash) -> Result<bool, RdyError> {
    let Some(open) = st.semantic().tension(pair) else {
        return Ok(false);
    };
    let mut first = open.record.clone();
    while let Some(prev) = first.prev {
        first = st.reader().semantic_tension(&prev)?;
    }
    Ok(first.review == *review && first.advice.is_some())
}

/// How many blocks the changes of `landing` write (create or update) that are in one of `areas`
/// in canon, and which of `areas` they are in. An absent block counts for none.
fn landed_in_areas(
    st: &ReadState,
    landing: &Hash,
    areas: &[String],
) -> Result<(usize, BTreeSet<String>), RdyError> {
    let reader = st.reader();
    let mut seen: BTreeSet<BlockId> = BTreeSet::new();
    let mut n = 0;
    let mut hit = BTreeSet::new();
    for c in reader.landing(landing)?.changes {
        for op in reader.change(&c)?.ops {
            let (Op::Create { block } | Op::Update { block, .. }) = op else {
                continue;
            };
            if !seen.insert(block.id.clone()) {
                continue;
            }
            let of = match st.areas_of(&Scope::Canon, &block.id) {
                Ok(a) => a,
                Err(e) if e.code == E_UNKNOWN_BLOCK => continue,
                Err(e) => return Err(e),
            };
            let mine: Vec<&String> = of.iter().filter(|a| areas.contains(a)).collect();
            if !mine.is_empty() {
                n += 1;
                hit.extend(mine.into_iter().cloned());
            }
        }
    }
    Ok((n, hit))
}

/// Whether request `q` waits for `actor`, as `ReadState::next_request` decides (leases aside):
/// it is not settled, and no item has `actor` among its authors. An item whose authors cannot be
/// read makes the request not count.
fn counts_for(st: &ReadState, actor: &str, q: &Request) -> bool {
    if st.semantic().settled(&q.key).is_some() {
        return false;
    }
    q.items.iter().all(|i| {
        st.item_authors_of(&q.key, i)
            .is_ok_and(|authors| !authors.iter().any(|a| a == actor))
    })
}
