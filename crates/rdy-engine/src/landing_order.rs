//! Order keys at a landing (D-TX3KH): a landing stops order-key ties by re-keying its own
//! blocks. Views that append under one parent from one base give their blocks one key; the
//! landing that would put a second block on a key gives each block it places a key of its own,
//! in its acceptance change. Blocks the landing does not place keep their keys, so no other
//! view's base goes stale. A tie that canon already holds (an older store, an imported tree)
//! stays as it is: the landing's blocks go after it.
use crate::Materialized;
use crate::error::*;
use rdy_model::{Block, BlockId, Op, decision};
use std::collections::{BTreeMap, BTreeSet};

/// The blocks that the applied ops `ops` place on `canon`: the ones they create, and the ones an
/// update puts under another parent or at another order key than canon holds. A body-only edit
/// places nothing.
pub fn placed(canon: &Materialized, ops: &[Op]) -> BTreeSet<BlockId> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Create { block } => Some(block.id.clone()),
            Op::Update { block, .. }
                if canon
                    .get(&block.id)
                    .is_none_or(|c| c.parent != block.parent || c.order != block.order) =>
            {
                Some(block.id.clone())
            }
            _ => None,
        })
        .collect()
}

/// The new order keys a landing gives the `placed` blocks of `candidate` (canon with the
/// landing's changes) that tie with a sibling: block → key.
///
/// Each parent of a placed block is read in `BlockId` order, its children in `(order, id)`
/// order, and each tied run that holds a placed block from left to right. The blocks of the run
/// the landing did not place keep their keys; a run of placed blocks only keeps its first (the
/// lowest id). The other placed blocks get keys after the run's key and before the next distinct
/// key, in their `(order, id)` order. So the landing makes no new tie, and the same inputs give
/// the same keys.
///
/// The next distinct key is the smaller of two: the next one in `candidate`, and the next one
/// above the run's key under the same parent in `view` (the proposing view, which the landing
/// rebases). So a block the view holds after the run but has not proposed (one written after the
/// propose) keeps its place after the re-keyed blocks: the re-key never ties with it or passes
/// it. With neither, there is no upper bound.
///
/// Refusals, before anything is written: `E_FROZEN` for a placed frozen decision that would need
/// a new key (a frozen decision is never re-keyed), and `E_ORDER_DEPTH` when the keys do not fit
/// between the two bounds under the key cap.
pub fn rekeys(
    canon: &Materialized,
    candidate: &Materialized,
    view: &Materialized,
    placed: &BTreeSet<BlockId>,
) -> Result<BTreeMap<BlockId, String>, RdyError> {
    let parents: BTreeSet<Option<BlockId>> = placed
        .iter()
        .filter_map(|id| candidate.get(id))
        .map(|b| b.parent.clone())
        .collect();
    let mut out = BTreeMap::new();
    for parent in parents {
        let kids = candidate.children(parent.as_ref());
        let in_view = view.children(parent.as_ref());
        let mut from = 0;
        while from < kids.len() {
            let mut to = from + 1;
            while to < kids.len() && kids[to].order == kids[from].order {
                to += 1;
            }
            let run = &kids[from..to];
            if run.len() > 1 && run.iter().any(|b| placed.contains(&b.id)) {
                let kept_one = run.iter().any(|b| !placed.contains(&b.id));
                // with no block of the run kept, the first placed one (the lowest id) stays
                let movers: Vec<&Block> = run
                    .iter()
                    .enumerate()
                    .filter(|(at, b)| placed.contains(&b.id) && (kept_one || *at > 0))
                    .map(|(_, b)| *b)
                    .collect();
                if let Some(frozen) = movers.iter().find(|b| {
                    decision::is_frozen(b) || canon.get(&b.id).is_some_and(decision::is_frozen)
                }) {
                    return Err(frozen_error(&frozen.id));
                }
                let key = run[0].order.as_str();
                let next_in_view = in_view.iter().map(|b| b.order.as_str()).find(|o| *o > key);
                let hi = match (kids.get(to).map(|b| b.order.as_str()), next_in_view) {
                    (Some(c), Some(v)) => Some(c.min(v)),
                    (c, v) => c.or(v),
                };
                let keys = rdy_model::order_run(Some(key), hi, movers.len())
                    .map_err(|_| depth_error(&movers[0].id))?;
                out.extend(movers.iter().map(|b| b.id.clone()).zip(keys));
            }
            from = to;
        }
    }
    Ok(out)
}

/// `E_STALE_BASE`: placed block `id` needs a new order key, but view `view` changed it after the
/// proposal.
pub(crate) fn stale_error(id: &BlockId, view: &str) -> RdyError {
    RdyError::new(
        E_STALE_BASE,
        format!("{id} needs a new order key, but it changed in view {view:?} after the proposal"),
        format!("propose again to include the later edits to {id}"),
    )
}

fn depth_error(id: &BlockId) -> RdyError {
    RdyError::new(
        E_ORDER_DEPTH,
        format!("cannot give {id} a distinct order key without changing another block"),
        format!("move {id} to a place with room, then propose again"),
    )
}

fn frozen_error(id: &BlockId) -> RdyError {
    RdyError::new(
        E_FROZEN,
        format!("{id} is frozen and needs a new order key"),
        "choose another place for the block, then propose again",
    )
}
