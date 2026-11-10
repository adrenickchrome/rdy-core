//! The suspect rows a read shows (D-AAQ9Z, D-01QX5; p-TFYXT): the graph's structural suspects,
//! each with the deferral a person accepted for it, the code suspects of the local checkout, and
//! (in [`ReadState::suspect_details`]) the suspects on cited blocks (repo links, D-Y49PX).
//! `rdy_graph::Suspect` and the stored TRANSITION stay structural: a code row has no target
//! block, and no deferral text is copied into a stored record.
use crate::foreign_refs::RepoLinks;
use crate::{RdyError, ReadState, Scope};
use rdy_graph::Suspect;
use rdy_model::foreign::ForeignId;
use rdy_model::plan::PlanAnswer;
use rdy_model::{Answer, BlockId, Hash, Provenance};
use rdy_store::kinds;
use rdy_store::plan::{AcceptedDeferral, MAX_DEFERRAL_PAIRS};
use rdy_store::{StoreError, StoreReader};
use serde::{Serialize, Serializer};
use std::collections::HashMap;
use std::collections::hash_map::Entry;

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

/// The target of a suspect row: a block of this project, or a cited block (repo links, D-Y49PX)
/// with the text a read shows for it, `<alias>:<id>` (the repo's first live alias, else its first
/// 12 hex digits, as every cited ref a read shows). Either serializes as that one string.
///
/// The order is a local block first, then a cited block by its shown text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowTarget {
    Block(BlockId),
    Cited { shown: String, target: ForeignId },
}

impl RowTarget {
    /// The local target block; none for a cited target.
    pub fn block(&self) -> Option<&BlockId> {
        match self {
            RowTarget::Block(b) => Some(b),
            RowTarget::Cited { .. } => None,
        }
    }
}

impl std::fmt::Display for RowTarget {
    /// The text a read shows: the block id, or the cited block's `<alias>:<id>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RowTarget::Block(b) => write!(f, "{b}"),
            RowTarget::Cited { shown, .. } => f.write_str(shown),
        }
    }
}

impl Serialize for RowTarget {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            RowTarget::Block(b) => b.serialize(s),
            RowTarget::Cited { shown, .. } => s.serialize_str(shown),
        }
    }
}

/// One suspect row as a read shows it. A structural row names its target block and may carry the
/// accepted `deferred`; a row on a cited block names it `<alias>:<id>` and carries no deferral; a
/// code row has a null target, the reason `code_changed` and its `paths` (`<repo>:<path>`,
/// sorted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SuspectReply {
    pub block: BlockId,
    pub target: Option<RowTarget>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredReply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

/// The rows in their one order: by block, then target (a local block, then a cited block by its
/// shown text, then null), then reason.
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
    /// deferrals, the rows on cited blocks ([`ReadState::foreign_suspects`], D-Y49PX), and, in
    /// canon, a view or the in-flight scope, the code rows.
    pub fn suspect_details(&self, scope: &Scope) -> Result<Vec<SuspectReply>, RdyError> {
        let structural = self.suspects(scope)?;
        let mut rows = self.suspect_rows(scope, structural, None)?;
        let cited = self.cited_rows(scope)?;
        if !cited.is_empty() {
            rows.extend(cited);
            sort_rows(&mut rows);
        }
        Ok(rows)
    }

    /// The suspects of `scope` on cited blocks as rows: each target shown by the alias of `scope`'s
    /// repo links, the reason as computed, and no deferral.
    fn cited_rows(&self, scope: &Scope) -> Result<Vec<SuspectReply>, RdyError> {
        let found = self.foreign_suspects(scope)?;
        if found.is_empty() {
            return Ok(vec![]);
        }
        let m = self.materialized(scope)?;
        let links = RepoLinks::of_view(&m);
        Ok(found
            .into_iter()
            .map(|s| SuspectReply {
                block: s.block,
                target: Some(RowTarget::Cited {
                    shown: format!("{}:{}", links.shown(&s.target.repo), s.target.id),
                    target: s.target,
                }),
                reason: s.reason,
                deferred: None,
                paths: None,
            })
            .collect())
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
        let mut cache = Decoded::default();
        for chunk in structural.chunks(MAX_DEFERRAL_PAIRS) {
            let pairs: Vec<(BlockId, BlockId)> = chunk
                .iter()
                .map(|s| (s.block.clone(), s.target.clone()))
                .collect();
            let found = reader.accepted_deferrals(&pairs, end)?;
            for (s, accepted) in chunk.iter().zip(found) {
                let deferred = match accepted {
                    Some(a) => Some(deferred_of(&a, reader, &mut cache)?),
                    None => None,
                };
                rows.push(SuspectReply {
                    block: s.block.clone(),
                    target: Some(RowTarget::Block(s.target.clone())),
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

/// A sealed plan decoded once: its seal's landing and its answers by item hash.
struct SealIndex {
    landing: Option<Hash>,
    answers: HashMap<Hash, PlanAnswer>,
}

/// The seals and answers a projection has decoded, each read from the store once however many
/// rows it explains.
#[derive(Default)]
struct Decoded {
    seals: HashMap<Hash, SealIndex>,
    answers: HashMap<Hash, Answer>,
}

impl Decoded {
    fn seal(&mut self, reader: &StoreReader, h: &Hash) -> Result<&SealIndex, StoreError> {
        match self.seals.entry(*h) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => {
                let p = reader.plan(h)?;
                let landing = p.seal.as_ref().map(|s| s.landing);
                let mut answers = HashMap::with_capacity(p.answers.len());
                for a in p.answers {
                    answers.entry(a.item).or_insert(a);
                }
                Ok(e.insert(SealIndex { landing, answers }))
            }
        }
    }

    fn answer(&mut self, reader: &StoreReader, h: &Hash) -> Result<&Answer, StoreError> {
        match self.answers.entry(*h) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => Ok(e.insert(reader.get_typed(kinds::ANSWER, h)?)),
        }
    }
}

/// The reply for an acceptance: a seal contributes the plan answer's author and reason, the
/// seal's landing and the accepting answer's person; a KeepSuspect answer contributes its
/// person, note and that person as the accepter.
fn deferred_of(
    a: &AcceptedDeferral,
    reader: &StoreReader,
    cache: &mut Decoded,
) -> Result<DeferredReply, StoreError> {
    match a {
        AcceptedDeferral::Plan {
            seal,
            item,
            answer: accepting,
            ..
        } => {
            let index = cache.seal(reader, seal)?;
            let given = index.answers.get(item).ok_or_else(|| {
                StoreError::Decode(format!("plan {seal}: no answer for a deferred item"))
            })?;
            let landing = index.landing.ok_or_else(|| {
                StoreError::Decode(format!("plan {seal}: a deferral names a plan with no seal"))
            })?;
            let (by, reason) = (given.by.clone(), given.reason.clone().unwrap_or_default());
            let accepted_by = cache.answer(reader, accepting)?.person.actor.clone();
            Ok(DeferredReply {
                by,
                reason,
                landing: Some(landing),
                accepted_by,
            })
        }
        AcceptedDeferral::KeepSuspect { answer: kept, .. } => {
            let a = cache.answer(reader, kept)?;
            Ok(DeferredReply {
                accepted_by: a.person.actor.clone(),
                reason: a.note.clone().unwrap_or_default(),
                by: a.person.clone(),
                landing: None,
            })
        }
    }
}
