//! Change plans in the store (D-4JMPH, p-CST21, p-FQZ2K, p-ZDH29): one version of a change plan
//! is an object of kind `PLAN` (`rdy_model::plan::Plan`). The ref `plan/<view>` names the latest
//! version of the view's chain.
//!
//! # The chain
//!
//! A chain's first version has neither `stable` nor `prev`; each later one names the chain's
//! first version and its immediate predecessor, of the same view. A sealed version ends the
//! chain: nothing follows it. The proposal version a chain last bound ends it too when that
//! version lands with no seal (O6), also when answer-only versions follow the bound one: the
//! view's next unsealed version starts a new chain, and none continues the ended one. The
//! binding of a proposal version that follows a version with no bound plan (O19) starts a new
//! chain too, whatever chain the view holds. Answers are cumulative, sorted by full item hash,
//! one (the latest) per item: a later version keeps every item its predecessor answers, and may
//! replace an item's answer or add items. A landing's plans (its seal, and the binding of the
//! final version it lands) keep every item the approved version's binding answers instead: they
//! carry that binding's answers, not the answer-only versions' after it. Heads are
//! sorted and distinct by repo. An answer-only version has no `proposal`; a bound version names
//! the proposal version it was written with, in the same batch, and every bound proposal has one
//! unsealed version. A seal names its landing, in the same batch before it, and that landing's
//! landed proposal version (`seal.version`, which `proposal` repeats). An unbound proposal's
//! seal has no earlier chain version (`stable` and `prev` absent).
//!
//! # Writers
//!
//! Only the typed plan writer ([`Store::put_plan`], answer-only versions) and the typed proposal
//! and landing composition (`Store::plan_frames`) write `PLAN` and `plan/` refs in a project.
//! The public `put`, `write`, `seal` and `set_refs` refuse both. A validated ingest or take
//! keeps them. A holder holds `PLAN` objects and their tables, but no `plan/` ref.
//!
//! # The tables (p-CST21)
//!
//! Four derived tables, from the frames alone (the same batch-index path as every other entry):
//! landing → its seal; decision and landing position → the seal; block and cause → the latest
//! accepted deferral; proposal → its bound version. A deferral row holds its source (a seal's
//! deferred item with its accepting ANSWER, or a KeepSuspect ANSWER), its log position and the
//! first position at which a canonical TRANSITION cleared the pair. A later acceptance replaces
//! the row; a clear keeps it and marks it, and a reopening does not unmark it.
//!
//! # Damage (closed)
//!
//! A record that breaks a rule is `plan <hash>: <reason>`, with the reason one of
//! [`INVALID_FIELDS`], [`INVALID_CHAIN`], [`INVALID_PROPOSAL`], [`INVALID_SEAL`],
//! [`UNSORTED_ANSWERS`], [`UNSORTED_HEADS`], [`UNSORTED_DECISIONS`], [`UNSORTED_ITEMS`] and
//! [`DUPLICATE_LANDING_SEAL`]: `Refused` from a writer, `Decode` from fsck.
use crate::Store;
use crate::bounded::{self, RecordError};
use crate::error::StoreError;
use crate::holder::StoreMode;
use crate::index::IndexEntry;
use crate::kinds;
use crate::redaction::StoredFrame;
use crate::segment::{HEADER as FRAME_HEADER, Location};
use crate::store::{Frames, StoreReader, encode};
use crate::transition::Transitions;
use rdy_model::engine_fields::MAX_REASON_CHARS;
use rdy_model::plan::{
    MAX_PLAN_ANSWERS, MAX_PLAN_BYTES, MAX_SEALED_ITEMS, Plan, PlanAct, PlanKind, PlanOutcome,
    item_hash, outcome_allowed, valid_git_hash,
};
use rdy_model::{Answer, AnswerAct, BlockId, Hash, LandingRecord, Proposal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub use crate::state::{
    PreparedProposal, ProposeRequest, Proposed, ReaffirmProposeRequest, ViewReads,
};

/// The ref prefix of change plans: `plan/<view>` names the view's latest plan version.
pub const PLAN_REF: &str = "plan/";

/// The ref of `view`'s latest plan version: `plan/<view>`.
pub fn plan_ref(view: &str) -> String {
    format!("{PLAN_REF}{view}")
}

/// The refusal of a `PLAN` frame outside the plan writers.
pub const PLAN_WRITER_ONLY: &str = "PLAN is written only by the plan writer";
/// The refusal of a `plan/` ref outside the plan writers.
pub const PLAN_REFS_WRITER_ONLY: &str = "plan refs are written only by the plan writer";

/// A field breaks its own rule: a text length, an enum combination, a git hash, a list bound.
pub const INVALID_FIELDS: &str = "invalid fields";
/// `stable` and `prev` do not name the chain this version follows, or the version drops an
/// item that its predecessor answers.
pub const INVALID_CHAIN: &str = "invalid chain";
/// The bound proposal is not a proposal of this view in this batch, or another version binds it.
pub const INVALID_PROPOSAL: &str = "invalid proposal";
/// The seal does not name its landing, its landed version or its accepting answers, or an item
/// has an outcome its kind cannot have (`rdy_model::plan::allowed_outcomes`).
pub const INVALID_SEAL: &str = "invalid seal";
pub const UNSORTED_ANSWERS: &str = "unsorted answers";
pub const UNSORTED_HEADS: &str = "unsorted heads";
pub const UNSORTED_DECISIONS: &str = "unsorted decisions";
pub const UNSORTED_ITEMS: &str = "unsorted items";
/// A second seal of one landing.
pub const DUPLICATE_LANDING_SEAL: &str = "duplicate landing seal";

/// The most decisions one seal names.
pub const MAX_SEAL_DECISIONS: usize = 100_000;
/// The most heads one version records.
pub const MAX_PLAN_HEADS: usize = 100_000;
/// The most bytes of a view name, a head's repo, a sealed item's subject or its edge.
pub const MAX_PLAN_NAME_BYTES: usize = 4096;
/// The most (block, cause) pairs one [`StoreReader::accepted_deferrals`] call reads.
pub const MAX_DEFERRAL_PAIRS: usize = 100_000;

/// The text of the rule that plan `h` breaks: `plan <h>: <reason>`.
pub fn damage(h: &Hash, reason: &str) -> String {
    format!("plan {h}: {reason}")
}

fn refused(h: &Hash, reason: &str) -> StoreError {
    StoreError::Refused(damage(h, reason))
}

/// The plan `payload` holds: the bounded decoder's record (HA1's limits and refusals), and
/// exactly its own canonical encoding (`Decode` otherwise), so a plan's hash is its id.
pub fn decode_plan(payload: &[u8]) -> Result<Plan, RecordError> {
    if payload.len() > MAX_PLAN_BYTES {
        return Err(RecordError::Limit {
            name: bounded::RECORD_BYTES,
            maximum: MAX_PLAN_BYTES as u64,
        });
    }
    let p: Plan = bounded::decode(payload)?;
    if !bounded::encodes_as(&p, payload) {
        return Err(RecordError::Decode);
    }
    Ok(p)
}

/// Refuses a `PLAN` frame and a `plan/` ref among `items`: the public writers' rule.
pub(crate) fn refuse_plan_writes(items: &[(u8, Vec<u8>)]) -> Result<(), StoreError> {
    for (kind, payload) in items {
        if *kind == kinds::PLAN {
            return Err(StoreError::Refused(PLAN_WRITER_ONLY.into()));
        }
        if *kind == kinds::REF_UPDATE
            && crate::holder::decode_ref(payload).is_some_and(|u| u.name.starts_with(PLAN_REF))
        {
            return Err(StoreError::Refused(PLAN_REFS_WRITER_ONLY.into()));
        }
    }
    Ok(())
}

/// Whether `keys` is strictly ascending: sorted and distinct.
fn ascending<T: Ord>(keys: impl IntoIterator<Item = T>) -> bool {
    let mut prev: Option<T> = None;
    for k in keys {
        if prev.as_ref().is_some_and(|p| *p >= k) {
            return false;
        }
        prev = Some(k);
    }
    true
}

/// A trimmed text of 1 to [`MAX_REASON_CHARS`] Unicode scalar values.
pub(crate) fn valid_reason(s: &str) -> bool {
    let chars = s.chars().count();
    s.trim() == s && (1..=MAX_REASON_CHARS).contains(&chars)
}

/// A name of 1 to [`MAX_PLAN_NAME_BYTES`] bytes.
fn valid_name(s: &str) -> bool {
    (1..=MAX_PLAN_NAME_BYTES).contains(&s.len())
}

/// The rule of the record `p` breaks by itself, if any: its fields, its lists' order and the
/// shape of its chain and seal. The one list that the writers, fsck and the sync batch check
/// apply.
pub(crate) fn invalid_record(p: &Plan) -> Option<&'static str> {
    if !valid_name(&p.view)
        || p.answers.len() > MAX_PLAN_ANSWERS
        || p.heads.len() > MAX_PLAN_HEADS
        || !p.answers.iter().all(|a| match a.act {
            PlanAct::Confirm | PlanAct::Defer => a.reason.as_deref().is_some_and(valid_reason),
            PlanAct::Done => a.reason.is_none(),
        })
        || !p
            .heads
            .iter()
            .all(|h| valid_name(&h.repo) && h.head.as_deref().is_none_or(valid_git_hash))
    {
        return Some(INVALID_FIELDS);
    }
    if let Some(seal) = &p.seal {
        let code = |k: PlanKind| k == PlanKind::UpdateImplementation;
        if seal.decisions.len() > MAX_SEAL_DECISIONS
            || seal.items.len() > MAX_SEALED_ITEMS
            || !seal
                .items
                .iter()
                .all(|i| valid_name(&i.subject) && valid_name(&i.edge))
        {
            return Some(INVALID_FIELDS);
        }
        if !ascending(seal.decisions.iter()) {
            return Some(UNSORTED_DECISIONS);
        }
        if !ascending(seal.items.iter().map(|i| i.id)) {
            return Some(UNSORTED_ITEMS);
        }
        let item_ok = |i: &rdy_model::plan::SealedItem| {
            i.id == item_hash(&i.cause, &i.subject, i.kind)
                && (code(i.kind) || BlockId::is_valid(&i.subject))
                && outcome_allowed(i.kind, i.outcome)
                && (i.answer.is_none() || (i.outcome == PlanOutcome::Deferred && !code(i.kind)))
        };
        if p.proposal != Some(seal.version) || !seal.items.iter().all(item_ok) {
            return Some(INVALID_SEAL);
        }
    }
    if !ascending(p.answers.iter().map(|a| a.item)) {
        return Some(UNSORTED_ANSWERS);
    }
    if !ascending(p.heads.iter().map(|h| h.repo.as_str())) {
        return Some(UNSORTED_HEADS);
    }
    if p.stable.is_some() != p.prev.is_some() {
        return Some(INVALID_CHAIN);
    }
    None
}

/// Where an object a plan names stands, as a check of the plan sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Earlier in the plan's own batch.
    Batch,
    /// In the log before the plan's batch.
    Log,
    /// Neither.
    Missing,
}

/// What a check of one plan reads around it: the writers' view of a batch being prepared, or
/// fsck's view of the log at the plan's frame.
pub(crate) trait PlanLookup {
    /// Where the object of `kind` and hash `h` stands.
    fn place(&self, kind: u8, h: &Hash) -> Result<Place, StoreError>;
    /// The payload of an object [`PlanLookup::place`] found.
    fn read(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError>;
    /// The latest plan version of `view` before this one, if any.
    fn latest(&self, view: &str) -> Result<Option<Hash>, StoreError>;
    /// The unsealed version bound to proposal `p` before this one, if any.
    fn bound(&self, p: &Hash) -> Result<Option<Hash>, StoreError>;
    /// The seal of landing `l` before this one, if any.
    fn sealed(&self, l: &Hash) -> Result<Option<Hash>, StoreError>;
    /// Whether a change of proposal version `p` landed before this plan (O6). Only a project's
    /// new-chain rule asks, so a holder's lookups answer `false`.
    fn landed(&self, _p: &Hash) -> Result<bool, StoreError> {
        Ok(false)
    }
    /// The landing of proposal version `p` before this plan: in its own batch, or (a holder)
    /// before it in the log. Only a landing's plans ask (the binding of its final version).
    fn landing_here(&self, p: &Hash) -> Result<Option<Hash>, StoreError>;
    /// Whether the store is a holder: its chains and seals arrive by take, with no view.
    fn holder(&self) -> bool;
}

/// The typed record of `kind` that `look` reads; None when it does not decode.
fn typed<T: serde::de::DeserializeOwned>(
    look: &dyn PlanLookup,
    kind: u8,
    h: &Hash,
) -> Result<Option<T>, StoreError> {
    match look.read(kind, h) {
        Ok(p) => Ok(bounded::decode::<T>(&p).ok()),
        Err(StoreError::Redacted { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The proposal version that the chain with the latest version `plan` last bound: the
/// `proposal` of the newest version from `plan` back through `prev` that names one (a seal names
/// its landed version), or `None` when no version of the chain does. `read` gives the version of
/// a hash (`None`: none that decodes, which ends the walk). The walk reads each answer-only
/// version after the last bound one, and no more.
fn last_bound(
    plan: &Plan,
    read: &dyn Fn(&Hash) -> Result<Option<Plan>, StoreError>,
) -> Result<Option<Hash>, StoreError> {
    let (mut proposal, mut prev) = (plan.proposal, plan.prev);
    while proposal.is_none() {
        let Some(at) = prev else {
            return Ok(None);
        };
        let Some(p) = read(&at)? else {
            return Ok(None);
        };
        (proposal, prev) = (p.proposal, p.prev);
    }
    Ok(proposal)
}

/// Whether the chain whose latest version is `head` has ended (O6): it is sealed, or the
/// proposal version its chain last bound landed (before the plan being checked) without a seal,
/// also when answer-only versions follow the bound one.
fn chain_ended(look: &dyn PlanLookup, head: &Plan) -> Result<bool, StoreError> {
    if head.seal.is_some() {
        return Ok(true);
    }
    let read = |h: &Hash| -> Result<Option<Plan>, StoreError> {
        look.read(kinds::PLAN, h).map(|b| decode_plan(&b).ok())
    };
    match last_bound(head, &read)? {
        Some(v) => look.landed(&v),
        None => Ok(false),
    }
}

/// The binding of the version a landing approved, when `p` is one of that landing's plans (its
/// seal, or the binding of the final version it lands) and the chain before `p` holds it: the
/// newest version from `q` (`p`'s predecessor) back through `prev` that names a proposal other
/// than the final version, when it binds the approved version. A landing's plans carry that
/// binding's answers, not answers the view recorded after it (D-4JMPH, p-8D6ES). `None`: `p` is
/// no landing's plan, or no such binding precedes it. The walk reads the answer-only versions
/// after the binding, as `last_bound` does.
fn approved_binding(look: &dyn PlanLookup, p: &Plan, q: &Plan) -> Result<Option<Plan>, StoreError> {
    let landing = match (&p.seal, p.proposal) {
        (Some(seal), _) => Some(seal.landing),
        (None, Some(v)) => look.landing_here(&v)?,
        (None, None) => None,
    };
    let Some(l) = landing else {
        return Ok(None);
    };
    let Some(landed) = typed::<LandingRecord>(look, kinds::LANDING, &l)? else {
        return Ok(None);
    };
    let approved = landed.approved_version.unwrap_or(landed.proposal);
    let mut at = q.clone();
    loop {
        match at.proposal {
            Some(v) if v == approved => return Ok(at.seal.is_none().then_some(at)),
            // the binding of the final version, earlier in the landing's batch
            Some(v) if v == landed.proposal => {}
            Some(_) => return Ok(None),
            None => {}
        }
        let Some(prev) = at.prev else {
            return Ok(None);
        };
        let Some(next) = look
            .read(kinds::PLAN, &prev)
            .map(|b| decode_plan(&b).ok())?
        else {
            return Ok(None);
        };
        at = next;
    }
}

/// Whether `p` binds a proposal version that follows a version with no bound plan (O19,
/// p-AEYTC): its binding is the first of a chain, whatever chain the view holds.
fn binds_after_unbound(look: &dyn PlanLookup, p: &Plan) -> Result<bool, StoreError> {
    let (None, Some(v)) = (&p.seal, p.proposal) else {
        return Ok(false);
    };
    let Some(version) = typed::<Proposal>(look, kinds::PROPOSAL, &v)? else {
        return Ok(false);
    };
    let Some(prev) = version.prev else {
        return Ok(false);
    };
    if look.place(kinds::PROPOSAL, &prev)? == Place::Missing {
        return Ok(false);
    }
    Ok(look.bound(&prev)?.is_none())
}

/// The rule plan `h` (the record `p`) breaks against what `look` reads, if any: its chain, its
/// bound proposal and its seal. A project needs a bound proposal and a seal's landing in the
/// plan's own batch, and its chain to continue the view's latest version; a holder needs them
/// before the plan.
pub(crate) fn invalid_context(
    h: &Hash,
    p: &Plan,
    look: &dyn PlanLookup,
) -> Result<Option<&'static str>, StoreError> {
    let holder = look.holder();
    let here = |place: Place| match place {
        Place::Batch => true,
        Place::Log => holder,
        Place::Missing => false,
    };
    match p.prev {
        Some(prev) => {
            if look.place(kinds::PLAN, &prev)? == Place::Missing {
                return Ok(Some(INVALID_CHAIN));
            }
            let Some(q) = look
                .read(kinds::PLAN, &prev)
                .map(|b| decode_plan(&b).ok())?
            else {
                return Ok(Some(INVALID_CHAIN));
            };
            // answers are cumulative: each of the predecessor's items keeps an answer (its act
            // and reason may be replaced); `p.answers` is sorted by item (`invalid_record`). A
            // landing's plans keep each item of the approved version's binding instead: an
            // answer the view recorded after that propose is not the version's
            let keeps = |item: &Hash| p.answers.binary_search_by(|a| a.item.cmp(item)).is_ok();
            let anchor = approved_binding(look, p, &q)?;
            let kept = &anchor.as_ref().unwrap_or(&q).answers;
            if q.view != p.view
                || q.seal.is_some()
                || p.stable != Some(q.stable.unwrap_or(prev))
                || !kept.iter().all(|a| keeps(&a.item))
                || (!holder && look.latest(&p.view)? != Some(prev))
            {
                return Ok(Some(INVALID_CHAIN));
            }
            // the binding of a version that follows an unbound one starts a chain (O19)
            if !holder && binds_after_unbound(look, p)? {
                return Ok(Some(INVALID_CHAIN));
            }
            // an unsealed version never continues a chain whose last bound proposal landed
            // without a seal (O6): the view's next version starts a chain
            if !holder && p.seal.is_none() && chain_ended(look, &q)? {
                return Ok(Some(INVALID_CHAIN));
            }
        }
        // a new chain: an unsealed version starts one only where no live chain stands (the
        // latest version is sealed, or the proposal its chain last bound landed without a
        // seal, O6), or when it binds a version that follows an unbound one (O19); an unbound
        // proposal's seal names no chain at all
        None if p.seal.is_none() && !holder && !binds_after_unbound(look, p)? => {
            if let Some(l) = look.latest(&p.view)? {
                let q = look.read(kinds::PLAN, &l).map(|b| decode_plan(&b).ok())?;
                let ended = match q {
                    Some(q) => chain_ended(look, &q)?,
                    None => false,
                };
                if !ended {
                    return Ok(Some(INVALID_CHAIN));
                }
            }
        }
        None => {}
    }
    if let Some(seal) = &p.seal {
        if !here(look.place(kinds::LANDING, &seal.landing)?) {
            return Ok(Some(INVALID_SEAL));
        }
        let landed = typed::<LandingRecord>(look, kinds::LANDING, &seal.landing)?;
        if landed.is_none_or(|l| l.proposal != seal.version)
            || look.place(kinds::PROPOSAL, &seal.version)? == Place::Missing
        {
            return Ok(Some(INVALID_SEAL));
        }
        for a in seal.items.iter().filter_map(|i| i.answer.as_ref()) {
            if look.place(kinds::ANSWER, a)? == Place::Missing {
                return Ok(Some(INVALID_SEAL));
            }
            let answer = typed::<Answer>(look, kinds::ANSWER, a)?;
            if answer.is_none_or(|a| a.act != AnswerAct::AcceptDeferrals) {
                return Ok(Some(INVALID_SEAL));
            }
        }
        if let Some(other) = look.sealed(&seal.landing)?
            && !(holder && other == *h)
        {
            return Ok(Some(DUPLICATE_LANDING_SEAL));
        }
    } else if let Some(prop) = p.proposal {
        if !here(look.place(kinds::PROPOSAL, &prop)?) {
            return Ok(Some(INVALID_PROPOSAL));
        }
        let version = typed::<Proposal>(look, kinds::PROPOSAL, &prop)?;
        if version.is_none_or(|v| v.view != p.view) {
            return Ok(Some(INVALID_PROPOSAL));
        }
        if let Some(other) = look.bound(&prop)?
            && other != *h
        {
            return Ok(Some(INVALID_PROPOSAL));
        }
    }
    Ok(None)
}

// -------------------------------------------------------------------------------------------
// the tables' rows

/// The source of an accepted deferral (p-CST21): pointers into the log, never copied text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcceptedDeferral {
    /// A seal's deferred item: the sealed PLAN, the item's full hash (its cumulative
    /// `PlanAnswer` is in that PLAN), the ANSWER that accepted the deferral, and the seal's log
    /// position.
    Plan {
        seal: Hash,
        item: Hash,
        answer: Hash,
        at: Location,
    },
    /// A person's KeepSuspect ANSWER and its log position.
    KeepSuspect { answer: Hash, at: Location },
}

impl AcceptedDeferral {
    /// The log position of the source.
    pub fn at(&self) -> Location {
        match self {
            AcceptedDeferral::Plan { at, .. } | AcceptedDeferral::KeepSuspect { at, .. } => *at,
        }
    }
}

/// One row of the deferral table: the latest acceptance of a (block, cause) pair and the first
/// position at which a TRANSITION cleared the pair after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeferralRow {
    pub(crate) source: AcceptedDeferral,
    pub(crate) cleared: Option<Location>,
}

/// The reducer of the deferral rows, which the index's apply and every fold use: an acceptance
/// replaces the row and its clear; a clear marks a row once and creates none.
pub(crate) fn reduce(row: Option<DeferralRow>, e: &IndexEntry) -> Option<DeferralRow> {
    match e {
        IndexEntry::DeferralAccept { source, .. } => Some(DeferralRow {
            source: *source,
            cleared: None,
        }),
        IndexEntry::DeferralClear { at, .. } => row.map(|r| DeferralRow {
            cleared: r.cleared.or(Some(*at)),
            ..r
        }),
        _ => row,
    }
}

/// The (block, cause) pair a deferral entry moves.
pub(crate) fn pair_of(e: &IndexEntry) -> Option<(&BlockId, &BlockId)> {
    match e {
        IndexEntry::DeferralAccept { block, cause, .. }
        | IndexEntry::DeferralClear { block, cause, .. } => Some((block, cause)),
        _ => None,
    }
}

/// What the plan entries of a frame read about earlier objects: the store, and the frames of
/// the batch being indexed (see [`Resolver`]).
pub(crate) trait Held {
    /// The block id of BLOCK version `v` that the store's index holds; None when it holds none.
    fn block_id(&self, v: &Hash) -> Result<Option<BlockId>, StoreError>;
    /// The block id of the BLOCK frame at `loc` (version `v`), which the index may not hold yet.
    fn block_id_at(&self, loc: Location, v: &Hash) -> Result<Option<BlockId>, StoreError>;
    /// Where the index holds LANDING `l`.
    fn landing_at(&self, l: &Hash) -> Result<Option<Location>, StoreError>;
}

impl Held for StoreReader {
    fn block_id(&self, v: &Hash) -> Result<Option<BlockId>, StoreError> {
        if !self.has_kind(kinds::BLOCK, v)? {
            return Ok(None);
        }
        Ok(Some(self.get_block(v)?.id))
    }

    fn block_id_at(&self, loc: Location, v: &Hash) -> Result<Option<BlockId>, StoreError> {
        match self.segments().read_hashed(loc) {
            Ok((kinds::BLOCK, found, payload)) if found == *v => {
                Ok(crate::store::decode::<rdy_model::Block>(&payload)
                    .ok()
                    .map(|b| b.id))
            }
            // a frame the walk did not hand out (a removed payload): its stub has the id
            _ => self.block_id(v),
        }
    }

    fn landing_at(&self, l: &Hash) -> Result<Option<Location>, StoreError> {
        self.index().typed(kinds::LANDING, l)
    }
}

/// A [`Held`] that holds nothing: for a pass that only checks that frames decode.
pub(crate) struct NoneHeld;

impl Held for NoneHeld {
    fn block_id(&self, _: &Hash) -> Result<Option<BlockId>, StoreError> {
        Ok(None)
    }
    fn block_id_at(&self, _: Location, _: &Hash) -> Result<Option<BlockId>, StoreError> {
        Ok(None)
    }
    fn landing_at(&self, _: &Hash) -> Result<Option<Location>, StoreError> {
        Ok(None)
    }
}

/// The resolver of one index pass (p-CST21: no second store walk): the store, through `held`,
/// and the BLOCK and LANDING frames already seen in this pass that the index does not hold yet.
/// Its buffers grow by fallible reservation, one entry per frame of the pass.
pub(crate) struct Resolver<'a> {
    held: &'a dyn Held,
    blocks: HashMap<Hash, Location>,
    landings: HashMap<Hash, Location>,
}

impl<'a> Resolver<'a> {
    pub(crate) fn new(held: &'a dyn Held) -> Resolver<'a> {
        Resolver {
            held,
            blocks: HashMap::new(),
            landings: HashMap::new(),
        }
    }

    /// Notes the frame of `kind` and `hash` at `loc`, once its entries are taken.
    pub(crate) fn note(&mut self, kind: u8, hash: Hash, loc: Location) -> Result<(), StoreError> {
        let map = match kind {
            kinds::BLOCK => &mut self.blocks,
            kinds::LANDING => &mut self.landings,
            _ => return Ok(()),
        };
        map.try_reserve(1)
            .map_err(|_| StoreError::Refused("the plan index pass could not grow".into()))?;
        map.entry(hash).or_insert(loc);
        Ok(())
    }

    /// Forgets the frames seen: the index now holds them.
    pub(crate) fn clear(&mut self) {
        self.blocks.clear();
        self.landings.clear();
    }

    fn block_id(&self, v: &Hash) -> Result<Option<BlockId>, StoreError> {
        match self.blocks.get(v) {
            Some(loc) => self.held.block_id_at(*loc, v),
            None => self.held.block_id(v),
        }
    }

    fn landing_at(&self, l: &Hash) -> Result<Option<Location>, StoreError> {
        match self.landings.get(l) {
            Some(loc) => Ok(Some(*loc)),
            None => self.held.landing_at(l),
        }
    }
}

/// The cause id of a KeepSuspect item, `reaffirm:<id>@<12 lowercase hex digits>`.
pub(crate) fn reaffirm_cause(item: &str) -> Option<BlockId> {
    let (id, v) = item.strip_prefix("reaffirm:")?.split_once('@')?;
    let hex = v.len() == 12 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    BlockId::from_string(id.to_string()).filter(|_| hex)
}

/// The plan table entries of the frame of `kind` with hash `hash` at `loc`: a PLAN's
/// ([`plan_entries`]), a KeepSuspect ANSWER's ([`answer_entries`]) and a TRANSITION's
/// ([`transition_entries`]); none for any other kind. The one derivation that live writes,
/// replays, fsck and the captured folds share. A PLAN that does not decode is `Decode`; an
/// ANSWER or TRANSITION that does not decode gives none (its own checks report it).
pub(crate) fn entries(
    kind: u8,
    hash: Hash,
    loc: Location,
    payload: &[u8],
    r: &Resolver<'_>,
) -> Result<Vec<IndexEntry>, StoreError> {
    match kind {
        kinds::PLAN => plan_entries(hash, loc, payload, r),
        kinds::ANSWER => answer_entries(hash, loc, payload, r),
        kinds::TRANSITION => Ok(match crate::store::decode::<Transitions>(payload) {
            Ok(t) => transition_entries(&t, loc),
            Err(_) => vec![],
        }),
        _ => Ok(vec![]),
    }
}

/// A PLAN's entries: an unsealed version's bound proposal; a seal's landing, each of its
/// decisions at the landing's position, and each deferred non-code item it seals with an
/// accepting ANSWER. A record that decodes and breaks a rule still gives them (fsck reports it).
pub(crate) fn plan_entries(
    hash: Hash,
    loc: Location,
    payload: &[u8],
    r: &Resolver<'_>,
) -> Result<Vec<IndexEntry>, StoreError> {
    let p = decode_plan(payload).map_err(|_| StoreError::Decode(damage(&hash, INVALID_FIELDS)))?;
    let mut out = vec![];
    let Some(seal) = &p.seal else {
        if let Some(proposal) = p.proposal {
            out.push(IndexEntry::PlanProposal {
                proposal,
                plan: hash,
                at: loc,
            });
        }
        return Ok(out);
    };
    out.push(IndexEntry::PlanSeal {
        landing: seal.landing,
        seal: hash,
        at: loc,
    });
    if let Some(landing_at) = r.landing_at(&seal.landing)? {
        out.extend(seal.decisions.iter().map(|d| IndexEntry::PlanDecision {
            decision: d.clone(),
            landing_at,
            seal: hash,
            at: loc,
        }));
    }
    for i in &seal.items {
        let (Some(answer), PlanOutcome::Deferred) = (i.answer, i.outcome) else {
            continue;
        };
        if i.kind == PlanKind::UpdateImplementation {
            continue;
        }
        if let Some(block) = BlockId::from_string(i.subject.clone()) {
            out.push(IndexEntry::DeferralAccept {
                block,
                cause: i.cause.clone(),
                source: AcceptedDeferral::Plan {
                    seal: hash,
                    item: i.id,
                    answer,
                    at: loc,
                },
            });
        }
    }
    Ok(out)
}

/// A KeepSuspect ANSWER's entries: one acceptance per member block its targets (BLOCK
/// versions) name, for the cause its item `reaffirm:<id>@<version12>` names. An answer that
/// breaks a rule gives none.
pub(crate) fn answer_entries(
    hash: Hash,
    loc: Location,
    payload: &[u8],
    r: &Resolver<'_>,
) -> Result<Vec<IndexEntry>, StoreError> {
    let Ok(a) = crate::store::decode::<Answer>(payload) else {
        return Ok(vec![]);
    };
    if a.act != AnswerAct::KeepSuspect || crate::answer::invalid(&a).is_some() {
        return Ok(vec![]);
    }
    let Some(cause) = reaffirm_cause(&a.item) else {
        return Ok(vec![]);
    };
    let mut blocks = BTreeSet::new();
    for t in &a.targets {
        if let Some(id) = r.block_id(t)? {
            blocks.insert(id);
        }
    }
    Ok(blocks
        .into_iter()
        .map(|block| IndexEntry::DeferralAccept {
            block,
            cause: cause.clone(),
            source: AcceptedDeferral::KeepSuspect {
                answer: hash,
                at: loc,
            },
        })
        .collect())
}

/// A TRANSITION's entries: a clear of each (block, target) pair its suspect moves make gone and
/// do not make new again. A reason-only replacement (gone and new on one pair) clears nothing.
pub(crate) fn transition_entries(t: &Transitions, loc: Location) -> Vec<IndexEntry> {
    let new: BTreeSet<(&BlockId, &BlockId)> = t.new.iter().map(|m| (&m.block, &m.target)).collect();
    let gone: BTreeSet<(&BlockId, &BlockId)> = t
        .gone
        .iter()
        .map(|m| (&m.block, &m.target))
        .filter(|p| !new.contains(p))
        .collect();
    gone.into_iter()
        .map(|(b, c)| IndexEntry::DeferralClear {
            block: b.clone(),
            cause: c.clone(),
            at: loc,
        })
        .collect()
}

// -------------------------------------------------------------------------------------------
// the writers

/// The writers' view of a batch being prepared: the store as it stands, the frames of its open
/// held batch, and the frames `items` that come before the plan in its own write.
struct WriteLookup<'a> {
    store: &'a Store,
    items: &'a [(u8, Vec<u8>)],
}

impl WriteLookup<'_> {
    /// The payload of `kind` and `h` among `items`.
    fn item(&self, kind: u8, h: &Hash) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(k, p)| *k == kind && Hash::of(p) == *h)
            .map(|(_, p)| &p[..])
    }

    /// The plans among `items`, in order.
    fn item_plans(&self) -> impl Iterator<Item = (Hash, Plan)> + '_ {
        self.items
            .iter()
            .filter(|(k, _)| *k == kinds::PLAN)
            .filter_map(|(_, p)| decode_plan(p).ok().map(|plan| (Hash::of(p), plan)))
    }
}

impl PlanLookup for WriteLookup<'_> {
    fn place(&self, kind: u8, h: &Hash) -> Result<Place, StoreError> {
        if self.item(kind, h).is_some() {
            return Ok(Place::Batch);
        }
        let open = self.store.open_batch().unwrap_or(&[]);
        if open.iter().any(|(k, f)| *k == kind && f == h) {
            return Ok(Place::Batch);
        }
        Ok(if self.store.has_kind(kind, h)? {
            Place::Log
        } else {
            Place::Missing
        })
    }

    fn read(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError> {
        match self.item(kind, h) {
            Some(p) => Ok(p.to_vec()),
            None => self.store.get_kind(kind, h),
        }
    }

    fn latest(&self, view: &str) -> Result<Option<Hash>, StoreError> {
        if let Some((h, _)) = self.item_plans().filter(|(_, p)| p.view == view).last() {
            return Ok(Some(h));
        }
        self.store
            .get_ref(&plan_ref(view))?
            .map(|v| crate::index::hash_from(&v, "plan ref"))
            .transpose()
    }

    fn bound(&self, p: &Hash) -> Result<Option<Hash>, StoreError> {
        let mut plans = self.item_plans();
        if let Some((h, _)) = plans.find(|(_, q)| q.seal.is_none() && q.proposal == Some(*p)) {
            return Ok(Some(h));
        }
        Ok(self.store.index().plan_proposal(p)?.map(|(h, _)| h))
    }

    fn sealed(&self, l: &Hash) -> Result<Option<Hash>, StoreError> {
        let mut plans = self.item_plans();
        if let Some((h, _)) = plans.find(|(_, q)| q.seal.as_ref().is_some_and(|s| s.landing == *l))
        {
            return Ok(Some(h));
        }
        Ok(self.store.index().plan_seal(l)?.map(|(h, _)| h))
    }

    fn landed(&self, p: &Hash) -> Result<bool, StoreError> {
        let Some(version) = typed::<Proposal>(self, kinds::PROPOSAL, p)? else {
            return Ok(false);
        };
        for c in &version.changes {
            if self.store.index().landing_of(c)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn landing_here(&self, p: &Hash) -> Result<Option<Hash>, StoreError> {
        Ok(self
            .items
            .iter()
            .filter(|(k, _)| *k == kinds::LANDING)
            .find(|(_, b)| bounded::decode::<LandingRecord>(b).is_ok_and(|l| l.proposal == *p))
            .map(|(_, b)| Hash::of(b)))
    }

    fn holder(&self) -> bool {
        matches!(self.store.mode(), Ok(StoreMode::Holder))
    }
}

impl Store {
    /// Writes the answer-only plan version `plan` and moves `plan/<view>` to it, in one batch
    /// (D-4JMPH). Returns its id. A version that binds a proposal or carries a seal is refused:
    /// only the proposal and landing composition writes those, beside their own records. A
    /// version that breaks a rule is refused (`plan <hash>: <reason>`), with nothing written.
    pub fn put_plan(&mut self, plan: &Plan) -> Result<Hash, StoreError> {
        let id = plan.id();
        if plan.seal.is_some() {
            return Err(refused(&id, INVALID_SEAL));
        }
        if plan.proposal.is_some() {
            return Err(refused(&id, INVALID_PROPOSAL));
        }
        let (id, items) = self.plan_frames(plan, &[])?;
        self.write_guarded(items)?;
        Ok(id)
    }

    /// The frames that write `plan` after `batch` (the earlier frames of its own write): the
    /// PLAN, then `plan/<view>` naming it. The plan is checked first (`check_plan`); nothing is
    /// written. The proposal and landing composition put these frames in their own batch.
    pub(crate) fn plan_frames(
        &self,
        plan: &Plan,
        batch: &[(u8, Vec<u8>)],
    ) -> Result<(Hash, Frames), StoreError> {
        let id = self.check_plan(plan, batch)?;
        let mut items = vec![(kinds::PLAN, encode(plan))];
        items.extend(self.ref_items_pub(vec![(plan_ref(&plan.view), id.0.to_vec())]));
        Ok((id, items))
    }

    /// Checks `plan` as the frame after `batch`: its encoding within the bounded decoder's
    /// limits, its own rules and its chain, bound proposal and seal against the store and the
    /// batch. Returns its id; a broken rule is `Refused` with `plan <hash>: <reason>`.
    pub(crate) fn check_plan(
        &self,
        plan: &Plan,
        batch: &[(u8, Vec<u8>)],
    ) -> Result<Hash, StoreError> {
        let bytes = encode(plan);
        let id = Hash::of(&bytes);
        if decode_plan(&bytes).is_err() {
            return Err(refused(&id, INVALID_FIELDS));
        }
        if let Some(reason) = invalid_record(plan) {
            return Err(refused(&id, reason));
        }
        let look = WriteLookup {
            store: self,
            items: batch,
        };
        if let Some(reason) = invalid_context(&id, plan, &look)? {
            return Err(refused(&id, reason));
        }
        Ok(id)
    }
}

// -------------------------------------------------------------------------------------------
// the reads

impl StoreReader {
    /// The plan version `h`, decoded within its bounds (`plan <h>: invalid fields` otherwise).
    pub fn plan(&self, h: &Hash) -> Result<Plan, StoreError> {
        let payload = match self.get_kind(kinds::PLAN, h) {
            Err(StoreError::NotFound(_)) if self.has(h)? => {
                let (k, _) = self.get_raw(h)?;
                return Err(StoreError::Decode(format!(
                    "{h} is not a plan (object kind {k})"
                )));
            }
            r => r?,
        };
        decode_plan(&payload).map_err(|_| StoreError::Decode(damage(h, INVALID_FIELDS)))
    }

    /// The proposal version that the chain with the latest version `plan` last bound: the
    /// `proposal` of the newest version from `plan` back through `prev` that names one, or
    /// `None` when none does (O6). It reads each answer-only version after the last bound one.
    pub fn last_bound_proposal(&self, plan: &Plan) -> Result<Option<Hash>, StoreError> {
        last_bound(plan, &|h| self.plan(h).map(Some))
    }

    /// The unsealed version bound to proposal version `proposal` whose frame lies before `end`
    /// (a captured log end). A seal never binds: an unbound proposal has none.
    pub fn plan_of_proposal(
        &self,
        proposal: &Hash,
        end: Location,
    ) -> Result<Option<Hash>, StoreError> {
        Ok(self
            .index()
            .plan_proposal(proposal)?
            .filter(|(_, at)| *at < end)
            .map(|(h, _)| h))
    }

    /// The seal of landing `landing` whose frame lies before `end`.
    pub fn plan_of_landing(
        &self,
        landing: &Hash,
        end: Location,
    ) -> Result<Option<Hash>, StoreError> {
        Ok(self
            .index()
            .plan_seal(landing)?
            .filter(|(_, at)| *at < end)
            .map(|(h, _)| h))
    }

    /// The seals that name decision `decision`, in their landings' log order, each before `end`.
    pub fn plans_of_decision(
        &self,
        decision: &BlockId,
        end: Location,
    ) -> Result<Vec<Hash>, StoreError> {
        Ok(self
            .index()
            .plan_decision_rows(decision)?
            .into_iter()
            .filter(|(_, at)| *at < end)
            .map(|(h, _)| h)
            .collect())
    }

    /// The latest acceptance of deferring `block`'s work owed to `cause`, as the log stood at
    /// `end`: none when no acceptance lies before `end`, or a TRANSITION before `end` cleared
    /// the pair after its latest acceptance there.
    pub fn accepted_deferral(
        &self,
        block: &BlockId,
        cause: &BlockId,
        end: Location,
    ) -> Result<Option<AcceptedDeferral>, StoreError> {
        let pair = [(block.clone(), cause.clone())];
        Ok(self.accepted_deferrals(&pair, end)?.pop().flatten())
    }

    /// [`StoreReader::accepted_deferral`] of each of `pairs` (block, cause), in order: at most
    /// [`MAX_DEFERRAL_PAIRS`]. A pair whose latest acceptance lies before `end` reads its table
    /// row; the pairs whose row is newer are folded over the log before `end`, batch by batch,
    /// with the table's own reducer and one optional value per pair.
    pub fn accepted_deferrals(
        &self,
        pairs: &[(BlockId, BlockId)],
        end: Location,
    ) -> Result<Vec<Option<AcceptedDeferral>>, StoreError> {
        if pairs.len() > MAX_DEFERRAL_PAIRS {
            return Err(StoreError::Refused(format!(
                "a deferral read takes at most {MAX_DEFERRAL_PAIRS} pairs"
            )));
        }
        let mut out = Vec::new();
        out.try_reserve_exact(pairs.len())
            .map_err(|_| StoreError::Refused("the deferral read could not grow".into()))?;
        let mut fold: BTreeMap<(BlockId, BlockId), Option<DeferralRow>> = BTreeMap::new();
        for (block, cause) in pairs {
            match self.index().deferral_row(block, cause)? {
                None => out.push(None),
                Some(r) if r.source.at() < end => {
                    out.push(r.cleared.is_none_or(|c| c >= end).then_some(r.source));
                }
                // the latest acceptance is past `end`: the pair is folded over the prefix
                Some(_) => {
                    fold.insert((block.clone(), cause.clone()), None);
                    out.push(None);
                }
            }
        }
        if fold.is_empty() {
            return Ok(out);
        }
        self.fold_deferrals(end, &mut fold)?;
        for (slot, pair) in out.iter_mut().zip(pairs) {
            if let Some(row) = fold.get(pair) {
                *slot = row.filter(|r| r.cleared.is_none()).map(|r| r.source);
            }
        }
        Ok(out)
    }

    /// Folds the deferral entries of the sealed batches before `end` into `rows`, for its pairs
    /// only: each batch's frames once, with a resolver of that batch's frames, and no batch kept.
    fn fold_deferrals(
        &self,
        end: Location,
        rows: &mut BTreeMap<(BlockId, BlockId), Option<DeferralRow>>,
    ) -> Result<(), StoreError> {
        let mut r = Resolver::new(self);
        self.for_each_batch(end, |b| {
            r.clear();
            let mut at = b.start;
            for f in &b.frames {
                let loc = at;
                let len = match f {
                    crate::dump::Frame::Present {
                        kind,
                        hash,
                        payload,
                    } => {
                        for e in entries(*kind, *hash, loc, payload, &r)? {
                            if let Some((b, c)) = pair_of(&e)
                                && let Some(row) = rows.get_mut(&(b.clone(), c.clone()))
                            {
                                *row = reduce(*row, &e);
                            }
                        }
                        r.note(*kind, *hash, loc)?;
                        payload.len() as u64
                    }
                    crate::dump::Frame::Dropped {
                        kind, hash, len, ..
                    } => {
                        r.note(*kind, *hash, loc)?;
                        u64::from(*len)
                    }
                };
                at.offset += FRAME_HEADER + len;
            }
            Ok(())
        })
    }
}

// -------------------------------------------------------------------------------------------
// fsck

/// fsck's view of the log at one plan's frame: the frames of the plan's batch before it, and
/// what the walk folded from the plans before it.
struct CheckLookup<'a> {
    reader: &'a StoreReader,
    holder: bool,
    batch_start: Location,
    batch: &'a [(u8, Hash)],
    latest: &'a HashMap<String, Hash>,
    bound: &'a HashMap<Hash, Hash>,
    sealed: &'a HashMap<Hash, Hash>,
}

impl PlanLookup for CheckLookup<'_> {
    fn place(&self, kind: u8, h: &Hash) -> Result<Place, StoreError> {
        if self.batch.iter().any(|(k, f)| *k == kind && f == h) {
            return Ok(Place::Batch);
        }
        Ok(match self.reader.index().typed(kind, h)? {
            Some(at) if at < self.batch_start => Place::Log,
            _ => Place::Missing,
        })
    }

    fn read(&self, kind: u8, h: &Hash) -> Result<Vec<u8>, StoreError> {
        self.reader.get_kind(kind, h)
    }

    fn latest(&self, view: &str) -> Result<Option<Hash>, StoreError> {
        Ok(self.latest.get(view).copied())
    }

    fn bound(&self, p: &Hash) -> Result<Option<Hash>, StoreError> {
        Ok(self.bound.get(p).copied())
    }

    fn sealed(&self, l: &Hash) -> Result<Option<Hash>, StoreError> {
        Ok(self.sealed.get(l).copied())
    }

    fn landed(&self, p: &Hash) -> Result<bool, StoreError> {
        let Some(version) = typed::<Proposal>(self, kinds::PROPOSAL, p)? else {
            return Ok(false);
        };
        for c in &version.changes {
            let Some(l) = self.reader.index().landing_of(c)? else {
                continue;
            };
            if self
                .reader
                .index()
                .typed(kinds::LANDING, &l)?
                .is_some_and(|at| at < self.batch_start)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn landing_here(&self, p: &Hash) -> Result<Option<Hash>, StoreError> {
        let lands = |l: &Hash| -> Result<bool, StoreError> {
            Ok(typed::<LandingRecord>(self, kinds::LANDING, l)?.is_some_and(|x| x.proposal == *p))
        };
        for (_, l) in self.batch.iter().filter(|(k, _)| *k == kinds::LANDING) {
            if lands(l)? {
                return Ok(Some(*l));
            }
        }
        if !self.holder {
            return Ok(None);
        }
        let Some(version) = typed::<Proposal>(self, kinds::PROPOSAL, p)? else {
            return Ok(None);
        };
        for c in &version.changes {
            if let Some(l) = self.reader.index().landing_of(c)?
                && self.place(kinds::LANDING, &l)? == Place::Log
                && lands(&l)?
            {
                return Ok(Some(l));
            }
        }
        Ok(None)
    }

    fn holder(&self) -> bool {
        self.holder
    }
}

impl StoreReader {
    /// The fsck rules of the change plans (closed; see the module docs), in log order up to what
    /// the index holds: every PLAN decodes, keeps its own rules and its chain, bound proposal
    /// and seal; in a project, every view's latest plan is its `plan/<view>` ref and every
    /// `plan/` ref names its view's latest plan; and the four plan tables hold exactly the rows
    /// the log gives. Returns how many PLAN records the log holds.
    pub(crate) fn check_plans(&self) -> Result<usize, StoreError> {
        let holder = matches!(self.mode(), Ok(StoreMode::Holder));
        let upto = self.index().indexed_upto()?;
        let origin = Location {
            segment: 0,
            offset: 0,
        };
        let mut n = 0usize;
        let mut batch: Vec<(u8, Hash)> = vec![];
        let mut batch_start = origin;
        let mut latest: HashMap<String, Hash> = HashMap::new();
        let mut bound: HashMap<Hash, Hash> = HashMap::new();
        let mut sealed: HashMap<Hash, Hash> = HashMap::new();
        // the derived rows the log gives, from the same entries and reducer as the index
        let want = crate::index::Index::in_memory()?;
        let mut rows: Vec<IndexEntry> = vec![];
        let mut r = Resolver::new(self);
        self.for_each_record_from(origin, upto, |at, frame, next| {
            let (kind, hash, payload) = match frame {
                StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } => (kind, hash, Some(payload)),
                StoredFrame::Dropped { kind, hash, .. } => (kind, hash, None),
            };
            if kind == kinds::BATCH_COMMIT {
                batch.clear();
                batch_start = next;
                r.clear();
                return Ok(());
            }
            if batch.is_empty() {
                batch_start = at;
            }
            if let Some(payload) = payload {
                if matches!(kind, kinds::PLAN | kinds::ANSWER | kinds::TRANSITION) {
                    let es = match entries(kind, hash, at, payload, &r) {
                        Ok(es) => es,
                        // an answer or transition that does not decode is their checks' to
                        // report
                        Err(_) if kind != kinds::PLAN => vec![],
                        Err(e) => return Err(e),
                    };
                    rows.extend(es);
                }
                if kind == kinds::PLAN {
                    let p = decode_plan(payload)
                        .map_err(|_| StoreError::Decode(damage(&hash, INVALID_FIELDS)))?;
                    if let Some(reason) = invalid_record(&p) {
                        return Err(StoreError::Decode(damage(&hash, reason)));
                    }
                    let look = CheckLookup {
                        reader: self,
                        holder,
                        batch_start,
                        batch: &batch,
                        latest: &latest,
                        bound: &bound,
                        sealed: &sealed,
                    };
                    if let Some(reason) = invalid_context(&hash, &p, &look)? {
                        return Err(StoreError::Decode(damage(&hash, reason)));
                    }
                    latest.insert(p.view.clone(), hash);
                    match &p.seal {
                        Some(s) => {
                            sealed.entry(s.landing).or_insert(hash);
                        }
                        None => {
                            if let Some(prop) = p.proposal {
                                bound.entry(prop).or_insert(hash);
                            }
                        }
                    }
                    n += 1;
                }
            } else if kind == kinds::PLAN {
                // a plan is never removable: a dropped one is damage
                return Err(StoreError::corrupt_at(at));
            }
            r.note(kind, hash, at)?;
            batch.push((kind, hash));
            Ok(())
        })?;
        if !holder {
            for (view, h) in &latest {
                let name = plan_ref(view);
                let named = self
                    .get_ref(&name)?
                    .map(|v| crate::index::hash_from(&v, "plan ref"))
                    .transpose()?;
                if named != Some(*h) {
                    return Err(StoreError::Decode(format!(
                        "plan ref {name:?} does not name the latest plan {h} of its view"
                    )));
                }
            }
            for (name, _) in self.refs_with_prefix(PLAN_REF)? {
                if !latest.contains_key(&name[PLAN_REF.len()..]) {
                    return Err(StoreError::Decode(format!(
                        "plan ref {name:?} names no plan of its view"
                    )));
                }
            }
        }
        want.apply(&rows, upto)?;
        if !want
            .plan_rows()?
            .agrees_before(&self.index().plan_rows()?, upto)
        {
            return Err(StoreError::Decode(
                "the plan tables differ from the plan records; run rdy reindex".into(),
            ));
        }
        Ok(n)
    }
}

/// The rows of the four plan tables, decoded (`Index::plan_rows`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanRows {
    /// landing -> (its seal, the seal's position)
    pub(crate) seals: BTreeMap<Hash, (Hash, Location)>,
    /// (decision, its landing's position) -> (the seal, the seal's position)
    pub(crate) decisions: BTreeMap<(BlockId, Location), (Hash, Location)>,
    /// proposal version -> (its bound plan, the plan's position)
    pub(crate) proposals: BTreeMap<Hash, (Hash, Location)>,
    /// (block, cause) -> the latest acceptance and its first clear
    pub(crate) deferrals: BTreeMap<(BlockId, BlockId), DeferralRow>,
}

impl PlanRows {
    /// Whether `have`, an index's rows, agrees with these rows (what the log before `upto`
    /// gives): every row of `have` whose source lies before `upto`, with a clear at or after
    /// `upto` read as none, is one of these, and these hold no other. A deferral row a writer
    /// replaced at or after `upto` cannot be compared, and is left out of both.
    pub(crate) fn agrees_before(&self, have: &PlanRows, upto: Location) -> bool {
        fn before<K: Ord + Clone>(
            m: &BTreeMap<K, (Hash, Location)>,
            upto: Location,
        ) -> BTreeMap<K, (Hash, Location)> {
            m.iter()
                .filter(|(_, (_, at))| *at < upto)
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        }
        if before(&have.seals, upto) != self.seals
            || before(&have.decisions, upto) != self.decisions
            || before(&have.proposals, upto) != self.proposals
        {
            return false;
        }
        let mut want = self.deferrals.clone();
        for (k, row) in &have.deferrals {
            let Some(w) = want.remove(k) else {
                if row.source.at() < upto {
                    return false;
                }
                continue;
            };
            if row.source.at() >= upto {
                continue;
            }
            let seen = DeferralRow {
                cleared: row.cleared.filter(|c| *c < upto),
                ..*row
            };
            if seen != w {
                return false;
            }
        }
        want.is_empty()
    }
}
