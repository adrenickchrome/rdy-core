//! A fork's store steps (D-99CWC, D-DZDB7, D-HX6BX): the fresh store's import of its source's
//! canon history, and the seed of its canon at the selected landing. A hub fork is a holder; a
//! project fork is a project (`Store::create_fork_project` makes it). A holder's origins are in
//! its creating batch; a project's are in its take's batch, after the records.
//!
//! The import takes one canon history with no base, written by the source's export. Its stream
//! names the source's repo, and the fork holds a new one. Only this path passes that
//! difference to the take (`Store::take_fork_history`). It runs every check of a pushed
//! history: each file, the union, the closure, each removal's portable proof and the
//! stale-content rule. It writes the records and the approved drops, and moves no ref.
//!
//! A holder's seed is a second batch through the move's seam: a checked plan with no new
//! record, and a seed with no expected tip. A project's seed is one batch of refs: `canon` and
//! the selected tags, and nothing else. Both name only the source's tags whose landing is on
//! the selected landing's `prev` chain. A tag that the history carries only as evidence (the
//! landing of a later removal or dispute correction) stays a stored record and gets no ref.
use crate::error::StoreError;
use crate::holder::{StoreMode, TAG_REF};
use crate::hub_canon::{CanonMove, CarriedTag, Moved};
use crate::kinds;
use crate::meta::Origin;
use crate::store::{Store, StoreReader};
use crate::sync_stream::StreamPurpose;
use crate::take_batch::{HistoryTaken, HistoryTarget, TakeError};
use rdy_model::{Hash, RepoId};
use std::collections::BTreeSet;
use std::fs::File;

impl Store {
    /// Takes the canon history at `landing` that the batch `files` carry into this fresh fork,
    /// a holder or a project. The stream's repo must be `source`, which differs from this
    /// store's. A store with canon, or with any batch after its creating batch, is refused, and
    /// so is a project with a tag or a view. A project's take writes `origins` last in its batch
    /// (D-HX6BX), so a removal it carries reads as fetched. A holder gives none: its origins are
    /// its creating batch's. See the module docs.
    pub fn import_fork<E>(
        &mut self,
        files: &[File],
        landing: Hash,
        source: RepoId,
        origins: &[Origin],
    ) -> Result<HistoryTaken, TakeError<E>> {
        let target = HistoryTarget {
            purpose: StreamPurpose::CanonHistory,
            root: landing,
        };
        self.take_fork_history(files, &target, source, origins)
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

impl Store {
    /// Seeds this project fork's canon at `landing`, which it stores, in one batch of refs:
    /// `canon`, then `tag/<name>` for each tag of `named` (the source's tags) whose landing is
    /// on `landing`'s chain, in name order. It writes no held, replaced, view or sync ref and no
    /// record. A holder, an open batch, a canon that is set already, and a landing the store
    /// lacks are refused, and nothing is written.
    pub fn seed_project_fork(
        &mut self,
        landing: Hash,
        named: &BTreeSet<Hash>,
    ) -> Result<(), StoreError> {
        self.ensure_writable()?;
        if self.reader().mode()? != StoreMode::Project {
            return Err(StoreError::Refused(
                "a project seed needs a project store".into(),
            ));
        }
        if self.open_batch().is_some() {
            return Err(crate::store::batch_open("before a project seed"));
        }
        let reader = self.reader();
        if reader.canon_head()?.is_some() {
            return Err(StoreError::Refused(
                "a project seed needs a store with no canon".into(),
            ));
        }
        if !reader.has_kind(kinds::LANDING, &landing)? {
            return Err(StoreError::NotFound(landing));
        }
        let chain = reader.canon_landings(&landing)?;
        let mut tags: Vec<(String, Hash)> = Vec::new();
        for h in named {
            if !reader.has_kind(kinds::TAG, h)? {
                continue;
            }
            let t = reader.read_tag(h)?;
            if chain.contains(&t.landing) {
                tags.push((format!("{TAG_REF}{}", t.name), *h));
            }
        }
        drop(reader);
        tags.sort();
        let mut refs = vec![("canon".to_string(), landing.0.to_vec())];
        refs.extend(tags.into_iter().map(|(n, h)| (n, h.0.to_vec())));
        let items = self.ref_items_pub(refs);
        let held = self.held();
        // no batch is open, so turning held batches off seals nothing: the seed is one batch
        self.set_held(false)?;
        // a seed receives history: it maps nothing (the next compact maps it)
        let wrote = self.without_map_pass(|s| s.write_guarded(items));
        self.set_held(held)?;
        wrote.map(drop)
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
