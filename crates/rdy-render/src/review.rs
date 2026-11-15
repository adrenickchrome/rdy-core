//! The review: what a draft changes against the canon it is based on. A draft render reads it to
//! mark the blocks a reader sees as edited or new, to show the text a draft removed where it
//! stood, and to count each doc's changes. The rule itself is the engine's
//! ([`rdy_engine::reader::review`]), which the publication review reads too; this module reads
//! the scopes a render compares and hands each pair of states to it.
use crate::prepare::BlockStatus;
use rdy_engine::reader::review as rule;
use rdy_engine::{Compare, CompareEntry, Materialized, RdyError, ReadState, Scope};
use rdy_model::BlockId;
use std::collections::{BTreeMap, BTreeSet};

pub use rdy_engine::reader::review::{DocChange, Mark, Removed, Review};

/// The scope a draft is read against: canon at the landing the view is based on, or canon itself
/// when that is canon's head (or the view floats). A proposal version (ls8) is read against canon
/// at the landing it is based on, or canon itself when that is canon's head. A version made before the first landing is based on the empty
/// canon: while nothing has landed that is canon, so it is reviewed as a view is (D-N5NSN, the
/// task-8 ruling); once canon has landed no scope names that base, and it has none. Canon and
/// as-of scopes have no base.
pub fn base_of(rdy: &ReadState, scope: &Scope) -> Option<Scope> {
    if let Scope::Proposal(p) = scope {
        // an unreadable version has no base here: its own read reports the error
        return match rdy.proposal_base(p).ok()? {
            // at canon's head that is canon itself: no as-of build of the same state
            Some(at) if Some(at) == rdy.canon_head() => Some(Scope::Canon),
            Some(at) => Some(Scope::At(at)),
            None if rdy.canon_head().is_none() => Some(Scope::Canon),
            None => None,
        };
    }
    let Scope::View(v) = scope else {
        return None;
    };
    match rdy.effective_base(v) {
        Some(b) if Some(b) != rdy.canon_head() => Some(Scope::At(b)),
        _ => Some(Scope::Canon),
    }
}

/// The review of `scope` against its base, or none when it has no base.
pub fn build(rdy: &ReadState, scope: &Scope) -> Result<Option<Review>, RdyError> {
    let Some(base) = base_of(rdy, scope) else {
        return Ok(None);
    };
    between(rdy, &base, scope).map(Some)
}

/// The changes from scope `from` to scope `to`, read as a review reads a view against its base:
/// the change summary of the landings since a tag reads it (sr6), so it counts by the same rule.
pub fn between(rdy: &ReadState, from: &Scope, to: &Scope) -> Result<Review, RdyError> {
    let (dm, bm) = (rdy.materialized(to)?, rdy.materialized(from)?);
    Ok(between_states(&bm, &dm))
}

/// The changes from state `from` to state `to`, by the rules of [`between`] (spv2, D-CWC91): a
/// review of a profile reads each side's selected state, and an empty side is
/// [`Materialized::default`]. A doc or folder that enters the selection is New and one that
/// leaves it is removed, even when its authored version is the same on both sides. The review
/// reads only the compare's state rows, never its conflicts, so no scope is needed.
pub(crate) fn between_states(from: &Materialized, to: &Materialized) -> Review {
    rule::between_states(from, to)
}

/// [`between_states`] for prepared sides: a block both sides show whose displayed status words
/// differ (`Planned`, `since <version>`) reads as edited, even when its selected block is the
/// same. The words come from each side's own preparation, so a built value changed on an excluded
/// decision counts.
pub(crate) fn between_prepared(
    from: &Materialized,
    to: &Materialized,
    from_status: &BTreeMap<BlockId, BlockStatus>,
    to_status: &BTreeMap<BlockId, BlockStatus>,
) -> Review {
    rule::between_prepared(from, to, from_status, to_status)
}

/// What a scope changes, as its change rows read it: the review against its base, else, for a
/// proposal version made on the empty project that no scope names, the review against that empty
/// project ([`unreviewed`]). The menu's count, the home page's list and the change summary all
/// read their rows from it (O12), so they agree. Review marks on the pages read only [`build`].
pub(crate) fn listed(rdy: &ReadState, scope: &Scope) -> Result<Option<Review>, RdyError> {
    match build(rdy, scope)? {
        Some(rv) => Ok(Some(rv)),
        None => unreviewed(rdy, scope),
    }
}

/// The count of [`Review::changed_blocks`] for a proposal version that has no base here: one
/// made before the first landing, read once canon has landed. It was made on the empty project,
/// so it is counted against that: every block it holds is added. Its page shows no marks
/// ([`base_of`]), but it still names the version and its count. Any other scope gives none.
pub fn unreviewed_changed_blocks(rdy: &ReadState, scope: &Scope) -> Result<Option<u32>, RdyError> {
    Ok(unreviewed(rdy, scope)?.map(|rv| rv.changed_blocks()))
}

/// The review behind [`unreviewed_changed_blocks`]: the proposal version against the empty
/// project. Its change summary lists every block it holds as new (sr6).
pub(crate) fn unreviewed(rdy: &ReadState, scope: &Scope) -> Result<Option<Review>, RdyError> {
    let Scope::Proposal(p) = scope else {
        return Ok(None);
    };
    if rdy.proposal_base(p)?.is_some() || rdy.canon_head().is_none() {
        return Ok(None);
    }
    let dm = rdy.materialized(scope)?;
    let empty = Materialized::default();
    let cmp = Compare {
        a: String::new(),
        b: String::new(),
        added: dm
            .iter()
            .map(|b| CompareEntry {
                id: b.id.to_string(),
                block_type: b.block_type.clone(),
                path: None,
                fields: vec![],
                from_parent: None,
                to_parent: None,
                conflict: None,
            })
            .collect(),
        deleted: vec![],
        changed: vec![],
        moved: vec![],
        conflicted: vec![],
    };
    Ok(Some(rule::of_compare(&cmp, &dm, &empty, &BTreeSet::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A status word that differs on a block both sides lack from the compare reads as nothing:
    /// only a block both sides hold counts, so a block that is added or removed is not doubled.
    #[test]
    fn rv2_a_status_change_needs_the_block_on_both_sides() {
        let none = Materialized::default();
        let planned = BlockStatus {
            planned: true,
            since: None,
        };
        let id = BlockId::parse("p-AAAAA").unwrap();
        let status = BTreeMap::from([(id, planned)]);
        let rv = between_prepared(&none, &none, &status, &BTreeMap::new());
        assert_eq!(rv.changed_blocks(), 0);
    }
}
