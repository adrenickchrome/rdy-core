//! Recording verdicts and semantic tension versions: write through the store, then publish the
//! pairs' keys and blocks (`Delta::Verdict`), so the next state re-reads only those pairs.
use crate::Rdy;
use crate::error::{E_BAD_INPUT, E_REVIEW_STALE, RdyError};
use crate::read_state::Delta;
use rdy_model::semantic::SemanticTensionV1;
use rdy_model::{BlockId, Hash, PairSide, Review, ReviewSubject, SemanticTension};
use rdy_store::StoreError;

impl Rdy {
    /// Records verdict `r` and, for a `tension` verdict, the semantic tension's first version `t`.
    /// Returns the review's id and the version's.
    ///
    /// Writes and publishes; it checks nothing a submit or an acceptance checks. Only `submit_verdict` and `accept_advice` call them outside tests.
    #[doc(hidden)]
    pub fn record_verdict(
        &mut self,
        r: &Review,
        t: Option<&SemanticTension>,
    ) -> Result<(Hash, Option<Hash>), RdyError> {
        self.record_verdict_v1(r, t.map(SemanticTensionV1::of).as_ref())
    }

    /// [`Rdy::record_verdict`] for a pair of any sides (repo links): a `ForeignPair` review and a
    /// [`SemanticTensionV1`].
    #[doc(hidden)]
    pub fn record_verdict_v1(
        &mut self,
        r: &Review,
        t: Option<&SemanticTensionV1>,
    ) -> Result<(Hash, Option<Hash>), RdyError> {
        let ids = self
            .store_mut()
            .put_verdict_v1(r, t)
            .map_err(|e| refused(e, E_REVIEW_STALE, "call rdy_review_next again"))?;
        // the store wrote it, so its subject is a pair
        let pairs: Vec<(Hash, Vec<PairSide>)> = match &r.subject {
            ReviewSubject::Pair { key, items } => {
                vec![(*key, rdy_model::semantic::local_sides(items))]
            }
            ReviewSubject::ForeignPair { key, items } => vec![(*key, items.clone())],
            _ => vec![],
        };
        self.publish(verdict_delta(pairs))?;
        Ok(ids)
    }

    /// Records later versions of semantic tensions (a person's acceptance), one batch for all.
    /// Returns the versions' ids, in order.
    ///
    /// Writes and publishes; it checks nothing a submit or an acceptance checks. Only `submit_verdict` and `accept_advice` call them outside tests.
    #[doc(hidden)]
    pub fn record_tension_versions<T: Clone + Into<SemanticTensionV1>>(
        &mut self,
        ts: &[T],
    ) -> Result<Vec<Hash>, RdyError> {
        // a tension of local sides, or of any sides (repo links)
        let ts: Vec<SemanticTensionV1> = ts.iter().cloned().map(Into::into).collect();
        self.record_tension_versions_v1(&ts)
    }

    /// [`Rdy::record_tension_versions`] for tensions of any sides (repo links).
    #[doc(hidden)]
    pub fn record_tension_versions_v1(
        &mut self,
        ts: &[SemanticTensionV1],
    ) -> Result<Vec<Hash>, RdyError> {
        let ids = self.store_mut().put_tension_versions_v1(ts).map_err(|e| {
            refused(
                e,
                E_BAD_INPUT,
                "read the tensions again and accept the current ones",
            )
        })?;
        if ids.is_empty() {
            return Ok(ids);
        }
        self.publish(verdict_delta(
            ts.iter().map(|t| (t.key, t.items.clone())).collect(),
        ))?;
        Ok(ids)
    }
}

/// A store refusal (`StoreError::Refused`: what the store's checks return) as `code` with the
/// store's message and `fix`; any other store error as it maps everywhere, so a stored object
/// that does not decode stays damage (`E_IO`).
fn refused(e: StoreError, code: &'static str, fix: &str) -> RdyError {
    match e {
        StoreError::Refused(message) => RdyError::new(code, message, fix),
        e => e.into(),
    }
}

/// `Delta::Verdict` for `pairs`: their keys, and their blocks sorted and distinct.
/// A cited side is no local block, so only the local sides' blocks are named.
fn verdict_delta(pairs: Vec<(Hash, Vec<PairSide>)>) -> Delta {
    let mut blocks: Vec<BlockId> = pairs
        .iter()
        .flat_map(|(_, items)| {
            items
                .iter()
                .filter_map(PairSide::local)
                .map(|i| i.block.clone())
        })
        .collect();
    blocks.sort();
    blocks.dedup();
    Delta::Verdict {
        keys: pairs.into_iter().map(|(k, _)| k).collect(),
        blocks,
    }
}
