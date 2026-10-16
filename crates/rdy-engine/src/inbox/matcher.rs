//! The closed rules that put a log event in a session's inbox (plan M2e2, ledger F4; D-TPZ0W,
//! D-V4NZX, D-Z0EMP). The logged tension and suspect moves are read apart (`inbox::moves`); only
//! the events below match here.
use super::Entry;
use super::inflight::RunInFlight;
use crate::gate::{ProposalRef, version_number};
use crate::semantic::candidates::Request;
use crate::tensions::TensionKind;
use crate::{E_UNKNOWN_BLOCK, Event, EventKind, RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash, Op, PairItem, Proposal, ReviewSubject};
use rdy_store::StoreError;
use rdy_store::inbox::InboxSub;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::transition::Cause;
use std::cmp::Reverse;
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
                pending_line(&pref),
            )
        }
        EventKind::ReviewSubmitted { review, pair, .. } => {
            // the verdict carries advice (read from the stored first version of the pair's
            // tension, which this review opened), and it waits on the proposal version that held
            // its items when it was written; both are read from records that never change and
            // from the log before the review, so landing the proposal, answering the advice,
            // publishing a later version or another proposal, or a pack change alters no entry
            if !verdict_carries_advice(st, review, pair)? {
                return Ok(None);
            }
            match first_holding(st, review, ev.seq)? {
                Some(pref) => entry(
                    "pending_added",
                    vec![review.to_string()],
                    pending_line(&pref),
                ),
                None => None,
            }
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

/// The proposal version that held the pair `review` judged when the review was written, read from
/// records that never change and bounded by the review's own place in the log.
///
/// Which changes held the judged items is read from the sides of the tension the review opened:
/// the review's own batch logs them (`TRANSITION`, cause `reviewed`), as the detector found them
/// when it wrote the verdict, under the tracking in force then. No content is hashed here, so a
/// pack change, a later edit of a block and a block with any number of fields change nothing.
/// A proposal counts only through its latest version written before the review, and only if none
/// of its changes had landed before the review. A version holds the pair for each item whose
/// block one of its changes writes (creates or updates) while that change is a side of the
/// tension. The proposal with the most items held wins, then the one whose version was written
/// last (the pair completes when the later of its items is written), then the smaller version
/// hash. A proposal published, replaced or landed after the review, a tension answered and a
/// pack change after the review change none of it, so the same verdict names the same version on
/// every read. A review of a pair that no proposal waiting at that time writes (an item in a view
/// that was never proposed, the other in canon) gives none.
fn first_holding(
    st: &ReadState,
    review: &Hash,
    batch_end: Location,
) -> Result<Option<ProposalRef>, RdyError> {
    let reader = st.reader();
    let ReviewSubject::Pair { items, .. } = reader.review(review)?.subject else {
        return Ok(None);
    };
    let Some(bound) = reader.location_of(kinds::REVIEW, review)? else {
        return Ok(None);
    };
    let sides = judged_changes(st, review, bound, batch_end)?;
    if sides.is_empty() {
        return Ok(None);
    }
    let mut best: Option<(Rank, Hash)> = None;
    for (key, latest) in reader.refs_with_prefix("proposal/")? {
        let (Some(stable), Ok(latest)) = (
            key.strip_prefix("proposal/")
                .and_then(|h| h.parse::<Hash>().ok()),
            <[u8; 32]>::try_from(latest.as_slice()).map(Hash),
        ) else {
            continue;
        };
        // the latest version written before the review
        let mut cur = Some(latest);
        let mut asof: Option<(Hash, Location, Proposal)> = None;
        while let Some(version) = cur {
            let p = reader.proposal(&version)?;
            match reader.location_of(kinds::PROPOSAL, &version)? {
                Some(at) if at < bound => {
                    asof = Some((version, at, p));
                    break;
                }
                _ => cur = p.prev,
            }
        }
        let Some((version, at, p)) = asof else {
            continue;
        };
        if landed_before(st, &p, bound)? {
            continue;
        }
        let score = holds_score(st, &p, &items, &sides)?;
        if score == 0 {
            continue;
        }
        let rank = (score, at, Reverse(version));
        if best.as_ref().is_none_or(|(b, _)| rank > *b) {
            best = Some((rank, stable));
        }
    }
    best.map(|((_, _, Reverse(version)), stable)| {
        Ok(ProposalRef {
            version,
            stable,
            n: version_number(st, &version)?,
        })
    })
    .transpose()
}

/// What ranks a proposal's version: the items it holds, where it was written in the log and, last,
/// the smaller hash first.
type Rank = (u32, Location, Reverse<Hash>);

/// Whether a change of `p` had landed before `bound` in the log.
fn landed_before(st: &ReadState, p: &Proposal, bound: Location) -> Result<bool, RdyError> {
    let reader = st.reader();
    for c in &p.changes {
        if let Some(landing) = reader.landing_of(c)?
            && reader
                .location_of(kinds::LANDING, &landing)?
                .is_some_and(|at| at < bound)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The in-flight changes that were sides of the semantic tensions `review` opened, read from the
/// `TRANSITION` record of the review's own batch (the frames from `review_at` to the batch's
/// commit, which ends before `batch_end`). A canon side holds no change. Empty when the batch
/// logged no such move.
fn judged_changes(
    st: &ReadState,
    review: &Hash,
    review_at: Location,
    batch_end: Location,
) -> Result<BTreeSet<Hash>, RdyError> {
    let reader = st.reader();
    let mut records = Vec::new();
    let mut committed = false;
    let walk = reader.for_each_frame_from(review_at, batch_end, |_, kind, hash, _, _| {
        if kind == kinds::BATCH_COMMIT {
            committed = true;
            // stops the walk at the end of the review's batch
            return Err(StoreError::Db("batch end".into()));
        }
        if kind == kinds::TRANSITION {
            records.push(hash);
        }
        Ok(())
    });
    if !committed {
        walk?;
    }
    let mut out = BTreeSet::new();
    for h in records {
        for m in reader.transitions(&h)?.opened {
            if m.kind == TensionKind::Semantic.name()
                && m.why.cause == Cause::Reviewed
                && m.why.id == Some(*review)
            {
                out.extend(m.sides.iter().filter_map(|s| s.change));
            }
        }
    }
    Ok(out)
}

/// How many items of a pair version `p` holds: the items whose block one of its changes writes
/// (creates or updates) while that change is one of the judged `sides`. Only the changes' ops are
/// read.
fn holds_score(
    st: &ReadState,
    p: &Proposal,
    items: &[PairItem],
    sides: &BTreeSet<Hash>,
) -> Result<u32, RdyError> {
    let reader = st.reader();
    let mut held = vec![false; items.len()];
    for c in p.changes.iter().filter(|c| sides.contains(c)) {
        for op in reader.change(c)?.ops {
            let (Op::Create { block } | Op::Update { block, .. }) = op else {
                continue;
            };
            for (i, item) in items.iter().enumerate() {
                if item.block == block.id {
                    held[i] = true;
                }
            }
        }
    }
    Ok(held.iter().filter(|h| **h).count() as u32)
}

/// The line of a `pending_added` entry.
fn pending_line(pref: &ProposalRef) -> String {
    format!("{pref} waits for an answer; read rdy_pending")
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
