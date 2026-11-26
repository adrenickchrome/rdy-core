//! The stored forms of a plan version (D-3PNRK, D-4JMPH, D-WWAK7). A PLAN of encoding 0 is the
//! frozen [`PlanV0`] body: every version written before plan deltas keeps its bytes and its id.
//! A PLAN of encoding [`crate::encoding::PLAN_DELTA_ENCODING`] is a [`PlanRecordV1`]: either a
//! whole version ([`PlanRecordV1::Full`], the frozen eight fields) or the difference from the
//! version immediately before it ([`PlanRecordV1::Delta`]).
//!
//! A delta keeps every metadata field of its version whole (stable, prev, view, proposal, heads,
//! provenance and seal) and records only how its answers differ: `answers_set` holds each new or
//! changed answer, `answers_remove` each item of the predecessor it no longer answers. Both
//! lists are strictly increasing by the full item hash, and disjoint. Removal is an encoding
//! operation, not permission to forget an answer: the plan rules decide which removals a
//! version may make, and the older record keeps the removed answer.
//!
//! These are wire types: each declaration's field and variant order is its wire format
//! (postcard). Never add, remove or reorder a field or a variant (D-F0904).
use crate::encoding::EncodedIdentity;
use crate::plan::{Plan, PlanAnswer, PlanHead, PlanSeal};
use crate::{Hash, Provenance};
use serde::{Deserialize, Serialize};

/// The most consecutive deltas that may follow a whole version: the next version after them is
/// whole again.
pub const MAX_PLAN_DELTA_DEPTH: usize = 128;

/// A plan version in encoding 0, and the body of a whole version in encoding 1: the eight fields
/// of the plan as released.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanV0 {
    pub stable: Option<Hash>,
    pub prev: Option<Hash>,
    pub view: String,
    pub proposal: Option<Hash>,
    pub answers: Vec<PlanAnswer>,
    pub heads: Vec<PlanHead>,
    pub provenance: Provenance,
    pub seal: Option<PlanSeal>,
}

impl PlanV0 {
    /// The frozen body of `p`.
    pub fn of(p: &Plan) -> PlanV0 {
        PlanV0 {
            stable: p.stable,
            prev: p.prev,
            view: p.view.clone(),
            proposal: p.proposal,
            answers: p.answers.clone(),
            heads: p.heads.clone(),
            provenance: p.provenance.clone(),
            seal: p.seal.clone(),
        }
    }

    /// The runtime plan, with the stored read identity `identity`.
    pub fn into_current(self, identity: Option<EncodedIdentity>) -> Plan {
        Plan {
            stable: self.stable,
            prev: self.prev,
            view: self.view,
            proposal: self.proposal,
            answers: self.answers,
            heads: self.heads,
            provenance: self.provenance,
            seal: self.seal,
            read_identity: identity,
        }
    }
}

/// The difference of a plan version from the version immediately before it (`prev`), in the
/// chain whose first version is `stable`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDeltaV1 {
    pub stable: Hash,
    pub prev: Hash,
    pub view: String,
    pub proposal: Option<Hash>,
    /// The new and changed answers, strictly increasing by item.
    pub answers_set: Vec<PlanAnswer>,
    /// The items of the predecessor this version no longer answers, strictly increasing.
    pub answers_remove: Vec<Hash>,
    pub heads: Vec<PlanHead>,
    pub provenance: Provenance,
    pub seal: Option<PlanSeal>,
}

impl PlanDeltaV1 {
    /// The delta of `next` from its predecessor `prev` (its stored id) whose answers are
    /// `prev_answers` (sorted by item), in the chain `stable`. An answer equal to its
    /// predecessor's is left out; an item only the predecessor answers is removed.
    pub fn between(stable: Hash, prev: Hash, prev_answers: &[PlanAnswer], next: &Plan) -> Self {
        let mut set = vec![];
        let mut remove = vec![];
        let (mut i, mut j) = (0, 0);
        while i < prev_answers.len() || j < next.answers.len() {
            match (prev_answers.get(i), next.answers.get(j)) {
                (Some(a), Some(b)) if a.item == b.item => {
                    if a != b {
                        set.push(b.clone());
                    }
                    i += 1;
                    j += 1;
                }
                (Some(a), Some(b)) if a.item < b.item => {
                    remove.push(a.item);
                    i += 1;
                }
                (Some(a), None) => {
                    remove.push(a.item);
                    i += 1;
                }
                (_, Some(b)) => {
                    set.push(b.clone());
                    j += 1;
                }
                (None, None) => break,
            }
        }
        PlanDeltaV1 {
            stable,
            prev,
            view: next.view.clone(),
            proposal: next.proposal,
            answers_set: set,
            answers_remove: remove,
            heads: next.heads.clone(),
            provenance: next.provenance.clone(),
            seal: next.seal.clone(),
        }
    }

    /// Whether both answer lists are strictly increasing by item and share no item.
    pub fn lists_valid(&self) -> bool {
        let increasing = |mut it: Box<dyn Iterator<Item = &Hash> + '_>| {
            let Some(mut prev) = it.next() else {
                return true;
            };
            for h in it {
                if h <= prev {
                    return false;
                }
                prev = h;
            }
            true
        };
        if !increasing(Box::new(self.answers_set.iter().map(|a| &a.item)))
            || !increasing(Box::new(self.answers_remove.iter()))
        {
            return false;
        }
        // both sorted: one merge walk finds a shared item
        let (mut i, mut j) = (0, 0);
        while i < self.answers_set.len() && j < self.answers_remove.len() {
            match self.answers_set[i].item.cmp(&self.answers_remove[j]) {
                std::cmp::Ordering::Equal => return false,
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
            }
        }
        true
    }
}

/// A PLAN of encoding 1. The variant order is the wire format: `Full` is 0, `Delta` 1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanRecordV1 {
    Full(PlanV0),
    Delta(PlanDeltaV1),
}

/// The borrowed form of [`PlanV0`]: the same fields in the same order, so its encoding is
/// [`PlanV0`]'s without copying the plan.
#[derive(Serialize)]
struct PlanV0Ref<'a> {
    stable: &'a Option<Hash>,
    prev: &'a Option<Hash>,
    view: &'a String,
    proposal: &'a Option<Hash>,
    answers: &'a Vec<PlanAnswer>,
    heads: &'a Vec<PlanHead>,
    provenance: &'a Provenance,
    seal: &'a Option<PlanSeal>,
}

/// The borrowed form of [`PlanRecordV1`], with the same variant order.
#[derive(Serialize)]
enum PlanRecordRef<'a> {
    Full(PlanV0Ref<'a>),
    Delta(&'a PlanDeltaV1),
}

fn v0_ref(p: &Plan) -> PlanV0Ref<'_> {
    PlanV0Ref {
        stable: &p.stable,
        prev: &p.prev,
        view: &p.view,
        proposal: &p.proposal,
        answers: &p.answers,
        heads: &p.heads,
        provenance: &p.provenance,
        seal: &p.seal,
    }
}

fn postcard_of<T: Serialize + ?Sized>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).expect("postcard encoding of an in-memory value cannot fail")
}

/// The encoding-0 body of `p`: the frozen [`PlanV0`] layout.
pub fn v0_body(p: &Plan) -> Vec<u8> {
    postcard_of(&v0_ref(p))
}

/// The encoding-1 body of `p` as a whole version: [`PlanRecordV1::Full`].
pub fn full_body(p: &Plan) -> Vec<u8> {
    postcard_of(&PlanRecordRef::Full(v0_ref(p)))
}

/// The encoding-1 body of the delta `d`: [`PlanRecordV1::Delta`].
pub fn delta_body(d: &PlanDeltaV1) -> Vec<u8> {
    postcard_of(&PlanRecordRef::Delta(d))
}
