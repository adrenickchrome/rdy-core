//! The suspect rows a read shows (D-AAQ9Z, D-01QX5; p-TFYXT): the graph's structural suspects,
//! each with the deferral a person accepted for it, and the code suspects of the local checkout.
//! `rdy_graph::Suspect` and the stored TRANSITION stay structural: a code row has no target
//! block, and no deferral text is copied into a stored record.
use crate::{RdyError, ReadState, Scope};
use rdy_graph::Suspect;
use rdy_model::{Answer, BlockId, Hash, Provenance};
use rdy_store::StoreError;
use rdy_store::kinds;
use rdy_store::plan::{AcceptedDeferral, MAX_DEFERRAL_PAIRS};
use serde::{Serialize, Serializer};
use std::collections::HashMap;

/// The reason of a code row.
pub const CODE_CHANGED: &str = "code_changed";

/// A deferral a person accepted for a suspect: who deferred it and why, the landing that sealed
/// the deferral (a plan's seal; none for a KeepSuspect answer), and who accepted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeferredReply {
    pub by: Provenance,
    pub reason: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "optional_hash"
    )]
    pub landing: Option<Hash>,
    pub accepted_by: String,
}

fn optional_hash<S: Serializer>(h: &Option<Hash>, s: S) -> Result<S::Ok, S::Error> {
    match h {
        Some(h) => s.serialize_str(&h.to_string()),
        None => s.serialize_none(),
    }
}

/// One suspect row as a read shows it. A structural row names its target block and may carry the
/// accepted `deferred`; a code row has a null target, the reason `code_changed` and its `paths`
/// (`<repo>:<path>`, sorted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SuspectReply {
    pub block: BlockId,
    pub target: Option<BlockId>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredReply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

/// The rows in their one order: by block, then target with null last, then reason.
fn sort_rows(rows: &mut [SuspectReply]) {
    rows.sort_by(|a, b| {
        (&a.block, a.target.is_none(), &a.target, &a.reason).cmp(&(
            &b.block,
            b.target.is_none(),
            &b.target,
            &b.reason,
        ))
    });
}

impl ReadState {
    /// Every suspect of `scope` as a read shows it: the structural rows with their accepted
    /// deferrals, and, in canon, a view or the in-flight scope, the code rows.
    pub fn suspect_details(&self, scope: &Scope) -> Result<Vec<SuspectReply>, RdyError> {
        let structural = self.suspects(scope)?;
        self.suspect_rows(scope, structural, None)
    }

    /// `structural` (already filtered as the caller reads them) as rows, with the accepted
    /// deferral of each as this state's captured log end shows it, and the code rows of `ids`
    /// (every block of the scope with `None`). A historical scope (a landing or a proposal)
    /// reads no checkout.
    pub(crate) fn suspect_rows(
        &self,
        scope: &Scope,
        structural: Vec<Suspect>,
        ids: Option<&[BlockId]>,
    ) -> Result<Vec<SuspectReply>, RdyError> {
        let mut rows = Vec::with_capacity(structural.len());
        let end = self.seq();
        let reader = self.reader();
        let mut seals: HashMap<Hash, rdy_model::plan::Plan> = HashMap::new();
        let mut answers: HashMap<Hash, Answer> = HashMap::new();
        for chunk in structural.chunks(MAX_DEFERRAL_PAIRS) {
            let pairs: Vec<(BlockId, BlockId)> = chunk
                .iter()
                .map(|s| (s.block.clone(), s.target.clone()))
                .collect();
            let found = reader.accepted_deferrals(&pairs, end)?;
            for (s, accepted) in chunk.iter().zip(found) {
                let deferred = match accepted {
                    Some(a) => Some(deferred_of(
                        &a,
                        &mut |h| plan_of(reader, &mut seals, h),
                        &mut |h| answer_of(reader, &mut answers, h),
                    )?),
                    None => None,
                };
                rows.push(SuspectReply {
                    block: s.block.clone(),
                    target: Some(s.target.clone()),
                    reason: s.reason.clone(),
                    deferred,
                    paths: None,
                });
            }
        }
        if matches!(scope, Scope::Canon | Scope::View(_) | Scope::Inflight) {
            for c in crate::reflects::code_suspects(self, scope, ids)? {
                rows.push(SuspectReply {
                    block: c.block,
                    target: None,
                    reason: CODE_CHANGED.to_string(),
                    deferred: None,
                    paths: Some(c.paths),
                });
            }
        }
        sort_rows(&mut rows);
        Ok(rows)
    }
}

fn plan_of(
    reader: &rdy_store::StoreReader,
    cache: &mut HashMap<Hash, rdy_model::plan::Plan>,
    h: &Hash,
) -> Result<rdy_model::plan::Plan, StoreError> {
    if let Some(p) = cache.get(h) {
        return Ok(p.clone());
    }
    let p = reader.plan(h)?;
    cache.insert(*h, p.clone());
    Ok(p)
}

fn answer_of(
    reader: &rdy_store::StoreReader,
    cache: &mut HashMap<Hash, Answer>,
    h: &Hash,
) -> Result<Answer, StoreError> {
    if let Some(a) = cache.get(h) {
        return Ok(a.clone());
    }
    let a: Answer = reader.get_typed(kinds::ANSWER, h)?;
    cache.insert(*h, a.clone());
    Ok(a)
}

/// The reply for an acceptance: a seal contributes the plan answer's author and reason, the
/// seal's landing and the accepting answer's person; a KeepSuspect answer contributes its
/// person, note and that person as the accepter.
fn deferred_of(
    a: &AcceptedDeferral,
    plan: &mut dyn FnMut(&Hash) -> Result<rdy_model::plan::Plan, StoreError>,
    answer: &mut dyn FnMut(&Hash) -> Result<Answer, StoreError>,
) -> Result<DeferredReply, StoreError> {
    match a {
        AcceptedDeferral::Plan {
            seal,
            item,
            answer: accepting,
            ..
        } => {
            let p = plan(seal)?;
            let given = p.answers.iter().find(|x| x.item == *item).ok_or_else(|| {
                StoreError::Decode(format!("plan {seal}: no answer for a deferred item"))
            })?;
            let landing = p.seal.as_ref().map(|s| s.landing).ok_or_else(|| {
                StoreError::Decode(format!("plan {seal}: a deferral names a plan with no seal"))
            })?;
            let accepted = answer(accepting)?;
            Ok(DeferredReply {
                by: given.by.clone(),
                reason: given.reason.clone().unwrap_or_default(),
                landing: Some(landing),
                accepted_by: accepted.person.actor,
            })
        }
        AcceptedDeferral::KeepSuspect { answer: kept, .. } => {
            let a = answer(kept)?;
            Ok(DeferredReply {
                accepted_by: a.person.actor.clone(),
                reason: a.note.unwrap_or_default(),
                by: a.person,
                landing: None,
            })
        }
    }
}
