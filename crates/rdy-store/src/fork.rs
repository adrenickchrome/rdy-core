//! A hub fork's store steps (D-99CWC, D-DZDB7, D-HX6BX): the fresh holder's import of its
//! source's canon history, and the seed of its canon at the selected landing.
//!
//! The import takes one canon history with no base, written by the source's export. Its stream
//! names the source's repo, and the fork holds a new one. Only this path passes that
//! difference to the take (`Store::take_fork_history`). It runs every check of a pushed
//! history: each file, the union, the closure, each removal's portable proof and the
//! stale-content rule. It writes the records and the approved drops, and moves no ref.
//!
//! The seed is a second batch through the move's seam: a checked plan with no new record, and
//! a seed with no expected tip. Its tag rule names only the source's tags whose landing is on
//! the selected landing's `prev` chain. A tag that the history carries only as evidence (the
//! landing of a later removal or dispute correction) stays a stored record and gets no ref.
use crate::error::StoreError;
use crate::hub_canon::{CanonMove, CarriedTag, Moved};
use crate::store::{Store, StoreReader};
use crate::sync_stream::StreamPurpose;
use crate::take_batch::{HistoryTaken, HistoryTarget, TakeError};
use rdy_model::{Hash, RepoId};
use std::collections::BTreeSet;
use std::fs::File;

impl Store {
    /// Takes the canon history at `landing` that the batch `files` carry into this fresh fork.
    /// The stream's repo must be `source`, which differs from this holder's. A holder with
    /// canon, or with any batch after its creating batch, is refused. See the module docs.
    pub fn import_fork<E>(
        &mut self,
        files: &[File],
        landing: Hash,
        source: RepoId,
    ) -> Result<HistoryTaken, TakeError<E>> {
        let target = HistoryTarget {
            purpose: StreamPurpose::CanonHistory,
            root: landing,
        };
        self.take_fork_history(files, &target, source)
    }

    /// Seeds this fork's canon at `landing`, which it stores, with no expected tip. The seed
    /// names each tag of `named` (the source's tags) whose landing is on `landing`'s chain, and
    /// no other. `at_ms` is the writer's clock. Returns the move's result: a canon that is set
    /// already gives `Moved::Behind`, and nothing is written.
    pub fn seed_fork(
        &mut self,
        landing: Hash,
        named: &BTreeSet<Hash>,
        at_ms: i64,
    ) -> Result<Moved, TakeError<StoreError>> {
        let batch = self.reader().check_move_batch(Vec::new(), landing)?;
        self.move_canon_with(&batch, &CanonMove::Seed, None, at_ms, |reader, carried| {
            fork_tags(reader, carried, &landing, named)
        })
    }
}

/// The tag rule of a fork's seed: of the `carried` tags, those that `named` holds and whose
/// landing is on `landing`'s `prev` chain, in the order given. The chain walk is the move's
/// own, with its depth bound.
pub fn fork_tags(
    reader: &StoreReader,
    carried: &[CarriedTag],
    landing: &Hash,
    named: &BTreeSet<Hash>,
) -> Result<Vec<Hash>, StoreError> {
    let chain = reader.canon_landings(landing)?;
    Ok(carried
        .iter()
        .filter(|c| named.contains(&c.hash) && chain.contains(&c.tag.landing))
        .map(|c| c.hash)
        .collect())
}
