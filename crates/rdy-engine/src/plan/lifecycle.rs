//! The change plan a proposal binds and a landing seals (D-4JMPH, system/store p-8D6ES,
//! concepts/briefings p-1FHVQ, p-QTQBT, p-AEYTC).
//!
//! # Propose
//!
//! Every explicit propose writes one plan version bound to the version it writes, in the same
//! batch: the candidate (the version's changes, with a key change rdy adds, on canon's head) is
//! computed and the request's plan answers are validated against it before anything is
//! appended. The version continues the view's live chain, keeping its first HEADs as the
//! baseline of the code items, or starts a chain; its HEADs are the ones this propose captured.
//! The version that follows one with no bound plan (O19) always starts a chain.
//! Its answers are cumulative and keep their own authors and content identities. The reply's
//! [`PlanSummary`] and warnings come from that same assessment.
//!
//! # Land
//!
//! A landing computes the selected version's complete outcome before anything is appended,
//! from its recorded canon, its recorded HEADs and its bound plan's answers (an answer the view
//! recorded after the propose is not the version's): each item's computed close, else its valid
//! answer (confirm is confirmed, done is done, defer is deferred), else open. An acceptance
//! change gives the landing a final version: an already-bound version's final version binds a
//! plan of its own in the landing's batch, with the approved candidate's answers, assessment
//! and HEADs. The seal names the landing and the version that landed, and carries the approved
//! version's cumulative answers. The landing's plans follow the view's chain, after any
//! answer-only version. A version with no item that re-affirms no decision writes no seal.
//!
//! A version proposed before store 17 has no bound plan (O19, p-AEYTC): its landing captures
//! the HEADs once as both ends, and its seal has no earlier chain version, no answers and no
//! answer pointer; each item is its computed close, else open. Its final version binds no plan.
use super::api::{
    ItemState, LiveItem, PlanAnswerInput, PlanCode, PlanCounts, PlanRow, StateCounts,
    W_PLAN_ANSWER_LAPSED, capture_heads, live_chain, live_rows, prepare_plan_answers,
};
use super::compute::{self, ComputedPlan};
use crate::alt::AltPick;
use crate::{RdyError, ReadState};
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem,
};
use rdy_model::{BlockId, Change, Hash, Proposal, Provenance};
use rdy_schema::{Issue, Severity};
use serde::Serialize;
use std::collections::BTreeMap;

/// Open non-code items of a proposed version (p-S3NTW); advisory until M3.
pub const W_PLAN_OPEN: &str = "W_PLAN_OPEN";
/// The fix of [`W_PLAN_OPEN`].
pub const PLAN_OPEN_FIX: &str = "close or defer each item: rdy_plan / rdy plan";
/// The most open items a proposal's summary lists.
pub const PLAN_SUMMARY_ITEMS: usize = 200;

/// What a propose names (p-RW856): the view, the title, the changes (`None`: the view's whole
/// set), the summary, the alternative it chooses, and the plan answers it records in the same
/// batch (none: a plain proposal, which still binds its plan).
#[derive(Clone, Copy, Debug)]
pub struct ProposePlanRequest<'a> {
    pub view: &'a str,
    pub title: &'a str,
    pub changes: Option<&'a [String]>,
    pub summary: Option<&'a str>,
    pub alternatives: Option<&'a AltPick>,
    pub plan_answers: &'a [PlanAnswerInput],
}

impl<'a> ProposePlanRequest<'a> {
    /// A request of `view` with `title` and nothing else.
    pub fn new(view: &'a str, title: &'a str) -> ProposePlanRequest<'a> {
        ProposePlanRequest {
            view,
            title,
            changes: None,
            summary: None,
            alternatives: None,
            plan_answers: &[],
        }
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The change plan a proposed version bound, as the propose reply shows it: the counts over
/// every item, code items included, the code files, and the first [`PLAN_SUMMARY_ITEMS`] open
/// items in full-hash order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanSummary {
    pub open: usize,
    pub closed: usize,
    pub deferred: usize,
    pub by_kind: BTreeMap<&'static str, StateCounts>,
    pub code: PlanCode,
    pub items: Vec<LiveItem>,
    /// The open items left out past [`PLAN_SUMMARY_ITEMS`].
    #[serde(skip_serializing_if = "is_zero")]
    pub items_omitted: usize,
}

impl Default for PlanSummary {
    fn default() -> PlanSummary {
        PlanSummary {
            open: 0,
            closed: 0,
            deferred: 0,
            by_kind: PlanCounts::default().by_kind,
            code: PlanCode::default(),
            items: vec![],
            items_omitted: 0,
        }
    }
}

/// The plan a propose binds, prepared before anything is appended: its chain links, its
/// cumulative answers, the HEADs captured for it, and what the reply shows.
pub(crate) struct ProposalPlan {
    stable: Option<Hash>,
    prev: Option<Hash>,
    answers: Vec<PlanAnswer>,
    heads: Vec<PlanHead>,
    pub(crate) summary: PlanSummary,
    pub(crate) warnings: Vec<Issue>,
}

impl ProposalPlan {
    /// The plan version bound to proposal version `version` of `view`, written by `by`.
    pub(crate) fn plan(&self, view: &str, version: Hash, by: Provenance) -> Plan {
        Plan {
            stable: self.stable,
            prev: self.prev,
            view: view.to_string(),
            proposal: Some(version),
            answers: self.answers.clone(),
            heads: self.heads.clone(),
            provenance: by,
            seal: None,
        }
    }
}

/// The summary and the warnings of `computed` with `answers`: the counts and open items of the
/// read rows, a lapse warning per lapsed answer (by item), then [`W_PLAN_OPEN`] when an open
/// item is not a code item.
fn assess(computed: &ComputedPlan, answers: &[PlanAnswer]) -> (PlanSummary, Vec<Issue>) {
    let (rows, counts, mut notes) = live_rows(computed, answers);
    notes.sort_by_key(|(item, _)| *item);
    let mut open_items = vec![];
    let mut open_docs = 0usize;
    for row in rows {
        let PlanRow::Live(item) = row else { continue };
        if item.state != ItemState::Open {
            continue;
        }
        if item.kind != PlanKind::UpdateImplementation {
            open_docs += 1;
        }
        open_items.push(item);
    }
    let omitted = open_items.len().saturating_sub(PLAN_SUMMARY_ITEMS);
    open_items.truncate(PLAN_SUMMARY_ITEMS);
    let summary = PlanSummary {
        open: counts.open,
        closed: counts.closed,
        deferred: counts.deferred,
        by_kind: counts.by_kind,
        code: PlanCode {
            files: computed.code_files,
            unavailable: computed.code_unavailable,
        },
        items: open_items,
        items_omitted: omitted,
    };
    let mut warnings: Vec<Issue> = notes
        .into_iter()
        .map(|(_, n)| Issue {
            severity: Severity::Warning,
            code: W_PLAN_ANSWER_LAPSED,
            block: None,
            message: n.message,
            fix: n.fix,
        })
        .collect();
    if open_docs > 0 {
        warnings.push(Issue {
            severity: Severity::Warning,
            code: W_PLAN_OPEN,
            block: None,
            message: format!("{open_docs} change-plan items are open"),
            fix: PLAN_OPEN_FIX.into(),
        });
    }
    (summary, warnings)
}

/// Prepares the plan a propose of `view` binds (see the module docs), writing nothing; its new
/// answers are `by`'s, the version's provenance: the HEADs captured now, the view's live chain,
/// the candidate `candidate` (the version's changes with rdy's key change, on canon's head) and
/// `inputs` validated against it, all before any frame is appended. `None` is a version with a
/// blocking conflict, which has no candidate until it is resolved: it binds the chain's answers
/// as they are, with an empty summary (the caller refuses its answers). `follows` is the
/// version the new one follows: when it has no bound plan (O19, p-AEYTC), the new plan is the
/// first of a chain, whatever chain the view holds, with no earlier answer and its own HEADs as
/// the baseline.
pub(crate) fn prepare_proposal_plan(
    st: &ReadState,
    by: &Provenance,
    view: &str,
    follows: Option<&Hash>,
    candidate: Option<&[Change]>,
    inputs: &[PlanAnswerInput],
) -> Result<ProposalPlan, RdyError> {
    let heads = capture_heads(st)?;
    let unbound = match follows {
        Some(v) => st.reader().plan_of_proposal(v, st.seq())?.is_none(),
        None => false,
    };
    let chain = if unbound { None } else { live_chain(st, view)? };
    let (stable, prev) = chain.as_ref().map_or((None, None), |c| c.next_links());
    let prior: &[PlanAnswer] = chain.as_ref().map_or(&[], |c| &c.plan.answers);
    let first = chain.as_ref().map_or(&heads, |c| &c.first_heads);
    let (answers, summary, warnings) = match candidate {
        Some(changes) => {
            let computed = compute::compute_changes(st, changes, st.canon_head(), &heads, first)?;
            let answers = prepare_plan_answers(st, by, view, &computed, prior, inputs)?;
            let (summary, warnings) = assess(&computed, &answers);
            (answers, summary, warnings)
        }
        None => (prior.to_vec(), PlanSummary::default(), vec![]),
    };
    Ok(ProposalPlan {
        stable,
        prev,
        answers,
        heads,
        summary,
        warnings,
    })
}

/// What a landing seals, computed before anything is appended (see the module docs).
pub(crate) struct LandingPlan {
    view: String,
    /// The approved version's bound plan's chain: the chain's first version and its latest one,
    /// which the landing's plans follow. `None` for a version with no bound plan (O19).
    chain: Option<(Hash, Hash)>,
    answers: Vec<PlanAnswer>,
    heads: Vec<PlanHead>,
    items: Vec<SealedItem>,
    decisions: Vec<BlockId>,
    /// Whether the landing writes a seal: the version has an item or re-affirms a decision.
    seals: bool,
    by: Provenance,
}

/// The sealed outcome of a computed item with `answers` (sorted by item): its computed close,
/// else its valid answer's, else open. An answer is valid when it names the item with its
/// current subject and cause content.
fn outcome_of(c: &compute::PlanCandidate, answers: &[PlanAnswer]) -> PlanOutcome {
    if let Some(close) = c.close {
        return close;
    }
    let Ok(i) = answers.binary_search_by(|a| a.item.cmp(&c.id)) else {
        return PlanOutcome::Open;
    };
    let a = &answers[i];
    if a.subject != c.subject_hash || a.cause != c.cause_hash {
        return PlanOutcome::Open;
    }
    match a.act {
        PlanAct::Confirm => PlanOutcome::Confirmed,
        PlanAct::Done => PlanOutcome::Done,
        PlanAct::Defer => PlanOutcome::Deferred,
        PlanAct::Dismiss => PlanOutcome::Dismissed,
    }
}

/// Prepares what the landing of proposal version `version` (`p`) seals, written by `by`
/// (the approval's provenance), writing nothing. `accepted` is the `AcceptDeferrals` ANSWER
/// bound to the approved version's deferral card (D-AAQ9Z): each deferred non-code item points
/// to it; a code item never does. A bound version uses its bound plan's answers
/// (cumulative to its propose: an answer the view recorded after it is not the version's),
/// its recorded HEADs and its chain's first HEADs, and its landing's plans follow the view's
/// chain; an unbound one the HEADs captured once, as both ends, and no answers (O19).
pub(crate) fn prepare_landing_plan(
    st: &ReadState,
    version: &Hash,
    p: &Proposal,
    by: &Provenance,
    accepted: Option<Hash>,
) -> Result<LandingPlan, RdyError> {
    let reader = st.reader();
    let (chain, answers, heads, first) = match reader.plan_of_proposal(version, st.seq())? {
        Some(bound) => {
            let b = reader.plan(&bound)?;
            let first = match b.stable {
                Some(s) => reader.plan(&s)?.heads,
                None => b.heads.clone(),
            };
            // the links follow the view's chain: the version's plan and any answer-only version
            // after it; the answers are the version's own
            let links = match live_chain(st, &p.view)? {
                Some(c) => {
                    let (stable, prev) = c.next_links();
                    stable.zip(prev)
                }
                None => Some((b.stable.unwrap_or(bound), bound)),
            };
            (links, b.answers, b.heads, first)
        }
        None => {
            let heads = capture_heads(st)?;
            (None, vec![], heads.clone(), heads)
        }
    };
    let computed = compute::compute(st, &p.view, &p.changes, p.canon_at, &heads, &first)?;
    let items: Vec<SealedItem> = computed
        .items
        .iter()
        .map(|c| {
            let outcome = outcome_of(c, &answers);
            let deferred =
                outcome == PlanOutcome::Deferred && c.kind != PlanKind::UpdateImplementation;
            SealedItem {
                id: c.id,
                kind: c.kind,
                subject: c.subject.clone(),
                cause: c.cause.clone(),
                edge: c.edge.clone(),
                outcome,
                // the acceptance of this version's deferral card (D-AAQ9Z)
                answer: if deferred { accepted } else { None },
            }
        })
        .collect();
    Ok(LandingPlan {
        view: p.view.clone(),
        seals: !items.is_empty() || computed.reaffirms,
        chain,
        answers,
        heads,
        items,
        decisions: computed.decisions,
        by: by.clone(),
    })
}

impl LandingPlan {
    /// The landing's plans: the plan bound to the final version `landed` when the landing
    /// writes one (`final_version`) and the approved version was bound, then the seal of
    /// `landing`, which lands `landed`, unless the version has no item and re-affirms nothing.
    pub(crate) fn plans(
        &self,
        landing: Hash,
        landed: Hash,
        final_version: bool,
    ) -> (Option<Plan>, Option<Plan>) {
        let bound = match self.chain {
            Some((stable, prev)) if final_version => Some(Plan {
                stable: Some(stable),
                prev: Some(prev),
                view: self.view.clone(),
                proposal: Some(landed),
                answers: self.answers.clone(),
                heads: self.heads.clone(),
                provenance: self.by.clone(),
                seal: None,
            }),
            _ => None,
        };
        if !self.seals {
            return (bound, None);
        }
        let (stable, prev) = match (&self.chain, &bound) {
            (Some((stable, _)), Some(b)) => (Some(*stable), Some(b.id())),
            (Some((stable, prev)), None) => (Some(*stable), Some(*prev)),
            (None, _) => (None, None),
        };
        let seal = Plan {
            stable,
            prev,
            view: self.view.clone(),
            proposal: Some(landed),
            answers: self.answers.clone(),
            heads: self.heads.clone(),
            provenance: self.by.clone(),
            seal: Some(PlanSeal {
                landing,
                version: landed,
                decisions: self.decisions.clone(),
                items: self.items.clone(),
            }),
        };
        (bound, Some(seal))
    }
}
