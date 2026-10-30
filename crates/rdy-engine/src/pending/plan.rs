//! Plan cards (D-AAQ9Z, O10, O19): the deferral card of a proposal version, and the binding a
//! plan card's hash carries beyond its text.
//!
//! # The deferral card
//!
//! A version whose immutable bound plan holds valid deferred non-code items stands beside its
//! batch with one `defer` item, ref `defer:P-x@n`. Its facts list each such item in full item
//! hash order: `<subject> (after <cause>)`, then `<by.actor>: <reason>`. An item the version
//! closes itself (a computed close) is never a deferral, and code items never enter the card
//! (their deferral needs no acceptance). The options are `o1` (accept) and `o2` (refuse), with
//! no recommendation, and `later`.
//!
//! A version with no bound plan (proposed before store 17, O19, p-AEYTC) has no deferral card
//! and no deferral hold: the store-16 readiness rules apply to it.
//!
//! # The binding (O10)
//!
//! The card's text may cut its facts to a budget; its hash never does. [`PlanCardBinding`] holds
//! the version, its bound plan and every listed item with its kind, subject, cause, their full
//! content identities and the complete plan answer. `card_hash` hashes it beside the rendered
//! card ([`super::card::PLAN_CARD_DOMAIN`]).
use super::item::{Fact, Item, ItemKind};
use super::queue::{item, later, option};
use super::reaffirm::ReaffirmMember;
use crate::gate::ProposalRef;
use crate::plan::api::{PlanCounts, capture_heads, live_rows};
use crate::plan::compute::{self, ComputedPlan};
use crate::plan::lifecycle::{PlanSummary, summary_of};
use crate::readiness::PlanSource;
use crate::{RdyError, ReadState};
use rdy_model::plan::{PlanAct, PlanAnswer, PlanKind};
use rdy_model::{BlockId, Hash, Proposal};
use serde::Serialize;
use std::collections::BTreeSet;

/// The title of a deferral card is `Deferrals in P-x@n`; its why is this line.
pub const DEFERRAL_WHY: &str = "these items stay open after this proposal lands";
/// The label of the deferral card's accepting option (`o1`).
pub const ACCEPT_LABEL: &str = "Accept the deferrals";
/// The label of the deferral card's refusing option (`o2`).
pub const REFUSE_LABEL: &str = "Refuse: do them in this proposal";

/// What a plan card binds beyond its text (O10). Each variant is the complete membership of
/// its card: every member and reason, also where the text leaves one out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanCardBinding {
    /// The deferral card of proposal version `version`, whose bound plan is `plan`: each
    /// deferred non-code item, by full item hash.
    Deferrals {
        #[serde(with = "rdy_store::transition::hex::one")]
        version: Hash,
        #[serde(with = "rdy_store::transition::hex::one")]
        plan: Hash,
        items: Vec<DeferralBinding>,
    },
    /// The re-affirmation card of `cause` at its full block version `version` (D-CQMA4): the
    /// cause's current retirement set (partial superseders included) and every member, with its
    /// full version and all its structural suspects ([`super::reaffirm`]).
    Reaffirm {
        cause: BlockId,
        #[serde(with = "rdy_store::transition::hex::one")]
        version: Hash,
        retirements: Vec<BlockId>,
        members: Vec<ReaffirmMember>,
    },
}

/// One deferred item a deferral card binds: its full item hash, kind, subject, cause, the full
/// content identities of the subject and the cause, and the complete plan answer that defers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeferralBinding {
    #[serde(with = "rdy_store::transition::hex::one")]
    pub item: Hash,
    pub kind: PlanKind,
    pub subject: String,
    pub cause: BlockId,
    #[serde(with = "rdy_store::transition::hex::opt")]
    pub subject_hash: Option<Hash>,
    #[serde(with = "rdy_store::transition::hex::one")]
    pub cause_hash: Hash,
    #[serde(serialize_with = "answer_wire")]
    pub answer: PlanAnswer,
}

/// A plan answer as a card binds it: the record's own encoding in a binary format (so the
/// card digest binds every field), and its hashes in hex in JSON.
fn answer_wire<S: serde::Serializer>(a: &PlanAnswer, s: S) -> Result<S::Ok, S::Error> {
    if !s.is_human_readable() {
        return a.serialize(s);
    }
    #[derive(Serialize)]
    struct Wire<'a> {
        item: String,
        act: PlanAct,
        reason: &'a Option<String>,
        subject: Option<String>,
        cause: String,
        by: &'a rdy_model::Provenance,
    }
    Wire {
        item: a.item.to_string(),
        act: a.act,
        reason: &a.reason,
        subject: a.subject.map(|h| h.to_string()),
        cause: a.cause.to_string(),
        by: &a.by,
    }
    .serialize(s)
}

/// A version's change plan as its cards and its readiness read it: the plan its record binds
/// (none before store 17), the counts over every item, code items included, the deferred
/// non-code items, the summary with the bound answers (`PlanSummary`, what the change-plan gate
/// reads), and the blocks its items name (each subject that is a block, and each cause; O18).
/// For the assessment (O17, O18): the (tension id, item) of each tension item a bound answer
/// answers, and the (subject, cause) of each reaffirm and confirm item whose subject is a block.
pub(crate) struct VersionPlan {
    pub(super) bound: Option<Hash>,
    pub(super) counts: PlanCounts,
    pub(super) deferrals: Vec<DeferralBinding>,
    pub(crate) summary: PlanSummary,
    pub(crate) blocks: BTreeSet<BlockId>,
    pub(crate) tension_answers: Vec<(Hash, Hash)>,
    /// The act and reason of each answer in `tension_answers`, by tension id (D-45RG6).
    pub(crate) tension_acts: Vec<(Hash, PlanAct, Option<String>)>,
    pub(crate) suspect_items: Vec<(BlockId, BlockId)>,
}

/// The change plan of version `version` (record `p`), whose plan comes from `source`, its tension
/// items those it was made with (ruling task-8 F1, [`compute::fixed_tensions`]): a bound version's
/// from its recorded canon, changes and HEADs against its chain's first HEADs, with its bound
/// plan's answers (an answer the view recorded after the propose is not the version's); an
/// unbound one's (O19) from the HEADs captured once as both ends, with no answer and so no
/// deferral.
pub(super) fn version_plan(
    st: &ReadState,
    version: &Hash,
    p: &Proposal,
    source: &PlanSource,
) -> Result<VersionPlan, RdyError> {
    let reader = st.reader();
    let fixed = compute::fixed_tensions(st, version)?;
    let read;
    let (h, plan, carried) = match source {
        PlanSource::Bound(None) => {
            let heads = capture_heads(st)?;
            let computed = compute::compute(
                st,
                &p.view,
                &p.changes,
                p.canon_at,
                fixed.as_ref(),
                &heads,
                &heads,
            )?;
            let (_, counts, _) = live_rows(&computed, &[]);
            return Ok(VersionPlan {
                bound: None,
                counts,
                deferrals: vec![],
                summary: summary_of(&computed, &[]),
                blocks: blocks_of(&computed),
                tension_answers: vec![],
                tension_acts: vec![],
                suspect_items: suspect_items(&computed),
            });
        }
        PlanSource::Bound(Some(h)) => {
            read = reader.plan(h)?;
            (*h, &read, None)
        }
        PlanSource::InHand {
            plan,
            hash,
            computed,
        } => (*hash, plan, computed.as_ref()),
    };
    let first = match plan.stable {
        Some(s) => reader.plan(&s)?.heads,
        None => plan.heads.clone(),
    };
    // the plan the propose computed for these inputs on this state, else computed now
    let fresh;
    let computed = match carried.and_then(|c| {
        c.on(
            st,
            &p.view,
            &p.changes,
            p.canon_at,
            fixed.as_ref(),
            &plan.heads,
            &first,
        )
    }) {
        Some(c) => c,
        None => {
            fresh = compute::compute(
                st,
                &p.view,
                &p.changes,
                p.canon_at,
                fixed.as_ref(),
                &plan.heads,
                &first,
            )?;
            &fresh
        }
    };
    let (_, counts, _) = live_rows(computed, &plan.answers);
    Ok(VersionPlan {
        bound: Some(h),
        counts,
        deferrals: deferrals(computed, &plan.answers),
        summary: summary_of(computed, &plan.answers),
        blocks: blocks_of(computed),
        tension_answers: tension_answers(computed, &plan.answers),
        tension_acts: tension_acts(computed, &plan.answers),
        suspect_items: suspect_items(computed),
    })
}

/// The (tension id, item) of each tension item of `computed` that one of `answers` (sorted by
/// item) answers: no computed close, and an answer that names the item with the tension's
/// current vkey (O5, O17).
fn tension_answers(computed: &ComputedPlan, answers: &[PlanAnswer]) -> Vec<(Hash, Hash)> {
    let mut out = vec![];
    for c in &computed.items {
        if c.kind != PlanKind::Tension || c.close.is_some() {
            continue;
        }
        let Ok(i) = answers.binary_search_by(|a| a.item.cmp(&c.id)) else {
            continue;
        };
        let a = &answers[i];
        if a.subject != c.subject_hash || a.cause != c.cause_hash {
            continue;
        }
        if let Ok(id) = c.subject.parse::<Hash>() {
            out.push((id, c.id));
        }
    }
    out
}

/// The (tension id, act, reason) of each answer [`tension_answers`] finds.
fn tension_acts(
    computed: &ComputedPlan,
    answers: &[PlanAnswer],
) -> Vec<(Hash, PlanAct, Option<String>)> {
    tension_answers(computed, answers)
        .into_iter()
        .filter_map(|(id, item)| {
            let i = answers.binary_search_by(|a| a.item.cmp(&item)).ok()?;
            Some((id, answers[i].act, answers[i].reason.clone()))
        })
        .collect()
}

/// The (subject, cause) of each reaffirm and confirm item of `computed` whose subject is a
/// block: the items whose subject can be suspect (O18).
fn suspect_items(computed: &ComputedPlan) -> Vec<(BlockId, BlockId)> {
    computed
        .items
        .iter()
        .filter(|c| matches!(c.kind, PlanKind::Reaffirm | PlanKind::Confirm))
        .filter_map(|c| Some((BlockId::parse(&c.subject).ok()?, c.cause.clone())))
        .collect()
}

/// The blocks the items of `computed` name: each subject that is a block id (never a code path
/// or a tension id), and each cause (O18).
fn blocks_of(computed: &ComputedPlan) -> BTreeSet<BlockId> {
    let mut out = BTreeSet::new();
    for c in &computed.items {
        out.insert(c.cause.clone());
        if !matches!(c.kind, PlanKind::UpdateImplementation | PlanKind::Tension)
            && let Ok(b) = BlockId::parse(&c.subject)
        {
            out.insert(b);
        }
    }
    out
}

/// The valid deferred non-code items of `computed` with `answers` (sorted by item), in full item
/// hash order: no computed close, an answer of act defer that names the item's current subject
/// and cause content.
fn deferrals(computed: &ComputedPlan, answers: &[PlanAnswer]) -> Vec<DeferralBinding> {
    let mut out = vec![];
    for c in &computed.items {
        if c.close.is_some() || c.kind == PlanKind::UpdateImplementation {
            continue;
        }
        let Ok(i) = answers.binary_search_by(|a| a.item.cmp(&c.id)) else {
            continue;
        };
        let a = &answers[i];
        if a.act != PlanAct::Defer || a.subject != c.subject_hash || a.cause != c.cause_hash {
            continue;
        }
        out.push(DeferralBinding {
            item: c.id,
            kind: c.kind,
            subject: c.subject.clone(),
            cause: c.cause.clone(),
            subject_hash: c.subject_hash,
            cause_hash: c.cause_hash,
            answer: a.clone(),
        });
    }
    out
}

/// The ref of the deferral card of `pref`.
pub(crate) fn defer_ref(pref: &ProposalRef) -> String {
    format!("defer:{pref}")
}

/// The deferral card of version `pref` beside its batch, when its bound plan `plan` holds a
/// deferred non-code item: see the module docs. Who raised it, its card and its hash are left
/// to the caller.
pub(super) fn deferral_item(pref: &ProposalRef, plan: &VersionPlan) -> Option<Item> {
    let bound = plan.bound?;
    if plan.deferrals.is_empty() {
        return None;
    }
    let r = pref.to_string();
    let mut it = item(
        ItemKind::Defer,
        &defer_ref(pref),
        &format!("Deferrals in {r}"),
    );
    it.why = DEFERRAL_WHY.to_string();
    it.batch = Some(r);
    it.facts = plan
        .deferrals
        .iter()
        .map(|d| Fact {
            name: format!("{} (after {})", d.subject, d.cause).into(),
            value: format!(
                "{}: {}",
                d.answer.by.actor,
                d.answer.reason.as_deref().unwrap_or_default()
            ),
        })
        .collect();
    it.options = vec![
        option(
            "o1",
            ACCEPT_LABEL,
            "The proposal may land with these items still open.",
            None,
        ),
        option(
            "o2",
            REFUSE_LABEL,
            "The proposal waits until its author closes the items and proposes again.",
            None,
        ),
        later(),
    ];
    it.plan_binding = Some(PlanCardBinding::Deferrals {
        version: pref.version,
        plan: bound,
        items: plan.deferrals.clone(),
    });
    Some(it)
}
