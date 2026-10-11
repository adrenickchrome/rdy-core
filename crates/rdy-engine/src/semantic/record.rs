//! Recording verdicts and semantic tension versions: write through the store, then publish the
//! pairs' keys and blocks (`Delta::Verdict`), so the next state re-reads only those pairs.
use crate::Rdy;
use crate::error::{E_BAD_INPUT, E_REVIEW_STALE, RdyError};
use crate::read_state::Delta;
use rdy_model::{BlockId, Hash, PairItem, Review, ReviewSubject, SemanticTension};
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
        let ids = self
            .store_mut()
            .put_verdict(r, t)
            .map_err(|e| refused(e, E_REVIEW_STALE, "call rdy_review_next again"))?;
        // the store wrote it, so its subject is a pair
        let pairs: Vec<(Hash, &[PairItem])> = match &r.subject {
            ReviewSubject::Pair { key, items } => vec![(*key, items.as_slice())],
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
    pub fn record_tension_versions(
        &mut self,
        ts: &[SemanticTension],
    ) -> Result<Vec<Hash>, RdyError> {
        let ids = self.store_mut().put_tension_versions(ts).map_err(|e| {
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
            ts.iter().map(|t| (t.key, t.items.as_slice())).collect(),
        ))?;
        Ok(ids)
    }
}

/// A store refusal (a decode error: what the store's checks return) as `code` with the store's
/// message and `fix`; any other store error as it maps everywhere.
fn refused(e: StoreError, code: &'static str, fix: &str) -> RdyError {
    match e {
        StoreError::Decode(message) => RdyError::new(code, message, fix),
        e => e.into(),
    }
}

/// `Delta::Verdict` for `pairs`: their keys, and their blocks sorted and distinct.
fn verdict_delta(pairs: Vec<(Hash, &[PairItem])>) -> Delta {
    let mut blocks: Vec<BlockId> = pairs
        .iter()
        .flat_map(|(_, items)| items.iter().map(|i| i.block.clone()))
        .collect();
    blocks.sort();
    blocks.dedup();
    Delta::Verdict {
        keys: pairs.into_iter().map(|(k, _)| k).collect(),
        blocks,
    }
}
