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
use crate::gate::ProposalRef;
use crate::plan::api::{PlanCounts, capture_heads, live_rows};
use crate::plan::compute::{self, ComputedPlan};
use crate::{RdyError, ReadState};
use rdy_model::plan::{PlanAct, PlanAnswer, PlanKind};
use rdy_model::{BlockId, Hash, Proposal};
use serde::Serialize;

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

/// A version's change plan as its cards read it: the plan its record binds (none before store
/// 17), the counts over every item, code items included, and the deferred non-code items.
pub(super) struct VersionPlan {
    pub(super) bound: Option<Hash>,
    pub(super) counts: PlanCounts,
    pub(super) deferrals: Vec<DeferralBinding>,
}

/// The change plan of version record `p`, whose bound plan is `bound`: a bound version's from
/// its recorded canon, changes and HEADs against its chain's first HEADs, with its bound plan's
/// answers (an answer the view recorded after the propose is not the version's); an unbound
/// one's (O19) from the HEADs captured once as both ends, with no answer and so no deferral.
pub(super) fn version_plan(
    st: &ReadState,
    p: &Proposal,
    bound: Option<Hash>,
) -> Result<VersionPlan, RdyError> {
    let reader = st.reader();
    let Some(h) = bound else {
        let heads = capture_heads(st)?;
        let computed = compute::compute(st, &p.view, &p.changes, p.canon_at, &heads, &heads)?;
        let (_, counts, _) = live_rows(&computed, &[]);
        return Ok(VersionPlan {
            bound: None,
            counts,
            deferrals: vec![],
        });
    };
    let plan = reader.plan(&h)?;
    let first = match plan.stable {
        Some(s) => reader.plan(&s)?.heads,
        None => plan.heads.clone(),
    };
    let computed = compute::compute(st, &p.view, &p.changes, p.canon_at, &plan.heads, &first)?;
    let (_, counts, _) = live_rows(&computed, &plan.answers);
    Ok(VersionPlan {
        bound: Some(h),
        counts,
        deferrals: deferrals(&computed, &plan.answers),
    })
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
