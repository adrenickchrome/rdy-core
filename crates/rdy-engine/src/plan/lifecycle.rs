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
//! When rdy's key change (K1) is written before the plan is computed (O1), the plan's tension
//! ids are the ones every later plan of the version reads. An answer the author gave to a
//! tension id from before that change is carried to the item of the same tension (the same kind
//! on the same blocks), with its act and reason; an input that names no item after the carry is
//! the unknown-item refusal, and one whose tension matches two items is not carried, so the item
//! stays open.
//!
//! # Land
//!
//! A landing computes the selected version's complete outcome before anything is appended,
//! from its recorded canon, its recorded HEADs and its bound plan's answers (an answer the view
//! recorded after the propose is not the version's): each item's computed close, else its valid
//! answer (confirm is confirmed, done is done, defer is deferred, dismiss is dismissed), else
//! open. Its tension items are those of its recorded canon too, so the seal holds the tension
//! items the propose showed. A tension item has no open seal form (D-45RG6): a landing whose
//! seal would hold an open one is not ready (`E_PROPOSAL_NOT_READY` with its `E_PLAN_OPEN`
//! issue), and writes nothing. An acceptance
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
    TENSION_DISPLAY_PREFIX, W_PLAN_ANSWER_LAPSED, capture_heads, live_chain, live_rows,
    prepare_plan_answers,
};
use super::compute::{self, ComputedPlan};
use crate::alt::AltPick;
use crate::error::E_PLAN_OPEN;
use crate::tensions::Tension;
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
/// The fix of `E_PLAN_OPEN` (D-ME2HD).
pub const PLAN_GATE_FIX: &str = "close or defer each item, and answer each tension item with confirm or dismiss: rdy_plan / rdy plan";
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
/// items in full-hash order. `gated` and `advised` are what [`plan_issues`] reads, counted over
/// every item before the cut; no reply shows them.
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
    /// The open non-code items on stable content, and the open tension items (D-FQ7AK, D-45RG6).
    #[serde(skip)]
    pub gated: usize,
    /// The open non-code items on drafts that are not tension items.
    #[serde(skip)]
    pub advised: usize,
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
            gated: 0,
            advised: 0,
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
    let (mut gated, mut advised) = (0usize, 0usize);
    for row in rows {
        let PlanRow::Live(item) = row else { continue };
        if item.state != ItemState::Open {
            continue;
        }
        match item.kind {
            // D-ERKCP: a code item never counts
            PlanKind::UpdateImplementation => {}
            PlanKind::Tension => gated += 1,
            _ if item.stable => gated += 1,
            _ => advised += 1,
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
        gated,
        advised,
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

/// The change-plan gate of `summary` (D-ME2HD, D-FQ7AK, D-45RG6): one [`E_PLAN_OPEN`] error that
/// counts the open non-code items on stable content and the open tension items (`gated`), then
/// one [`W_PLAN_OPEN`] warning that counts the other open non-code items, on drafts
/// (`advised`). Each comes only when its count is above 0; a code item never counts (D-ERKCP).
pub fn plan_issues(summary: &PlanSummary) -> Vec<Issue> {
    let mut out = vec![];
    if summary.gated > 0 {
        out.push(plan_open_issue(summary.gated));
    }
    if summary.advised > 0 {
        out.push(Issue {
            severity: Severity::Warning,
            code: W_PLAN_OPEN,
            block: None,
            message: format!("{} change-plan items are open", summary.advised),
            fix: PLAN_OPEN_FIX.into(),
        });
    }
    out
}

/// The [`E_PLAN_OPEN`] issue for `n` gated items.
fn plan_open_issue(n: usize) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_PLAN_OPEN,
        block: None,
        message: format!("{n} change-plan items are open"),
        fix: PLAN_GATE_FIX.into(),
    }
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
    prepare_proposal_plan_keyed(st, None, by, view, follows, candidate, inputs)
}

/// The state and the changes of a version before rdy's key change was written: the tension ids
/// its author saw and answered (see the module docs).
pub(crate) struct BeforeKeyChange<'a> {
    pub(crate) st: &'a ReadState,
    pub(crate) changes: &'a [Change],
}

/// [`prepare_proposal_plan`] on `st`, where rdy's key change is written and in `candidate`,
/// with the answers to the tension items of `before` carried (see the module docs).
pub(crate) fn prepare_proposal_plan_keyed(
    st: &ReadState,
    before: Option<BeforeKeyChange<'_>>,
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
            let computed =
                compute::compute_changes(st, view, changes, st.canon_head(), &heads, first)?;
            let (prior, inputs) = match &before {
                Some(b) => carry_answers(b, view, &computed, prior, inputs)?,
                None => (prior.to_vec(), inputs.to_vec()),
            };
            let answers = prepare_plan_answers(st, by, view, &computed, &prior, &inputs)?;
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

/// `prior` and `inputs` with each answer to a tension item of `before` that `computed` has
/// under another id carried to it (see the module docs).
fn carry_answers(
    before: &BeforeKeyChange<'_>,
    view: &str,
    computed: &ComputedPlan,
    prior: &[PlanAnswer],
    inputs: &[PlanAnswerInput],
) -> Result<(Vec<PlanAnswer>, Vec<PlanAnswerInput>), RdyError> {
    let now: BTreeMap<Hash, &Tension> = computed.tensions.iter().map(|t| (t.id, &**t)).collect();
    // each earlier tension that the plan no longer has: the one item of the same tension, or
    // `None` when two match
    let mut carried: BTreeMap<Hash, (Tension, Option<&Tension>)> = BTreeMap::new();
    for t in compute::tension_set(before.st, view, before.changes)? {
        if now.contains_key(&t.id) {
            continue;
        }
        let mut same = computed
            .tensions
            .iter()
            .filter(|n| compute::shape(n) == compute::shape(&t));
        let to = match (same.next(), same.next()) {
            (Some(n), None) => Some(&**n),
            _ => None,
        };
        carried.insert(t.id, ((*t).clone(), to));
    }
    if carried.is_empty() {
        return Ok((prior.to_vec(), inputs.to_vec()));
    }
    let item_of = |t: &Tension| compute::tension_candidate(t, None);
    // the view's recorded answers: one that names an earlier item with its tension's vkey moves
    // to the later item, unless that item has an answer already
    let mut answers: BTreeMap<Hash, PlanAnswer> =
        prior.iter().map(|a| (a.item, a.clone())).collect();
    for (from, to) in carried.values() {
        let (Some(to), Some(old)) = (to, item_of(from)) else {
            continue;
        };
        let Some(new) = item_of(to) else { continue };
        if answers.contains_key(&new.id) {
            continue;
        }
        if let Some(a) = answers
            .get(&old.id)
            .filter(|a| a.cause == from.vkey)
            .cloned()
        {
            answers.remove(&old.id);
            answers.insert(
                new.id,
                PlanAnswer {
                    item: new.id,
                    cause: new.cause_hash,
                    ..a
                },
            );
        }
    }
    // the request's answers: a tn- id that names no item now but one earlier tension
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        let Some(hex) = input
            .item
            .strip_prefix(TENSION_DISPLAY_PREFIX)
            .filter(|h| !now.keys().any(|id| id.to_string().starts_with(*h)))
        else {
            out.push(input.clone());
            continue;
        };
        let mut from = carried
            .iter()
            .filter(|(id, _)| id.to_string().starts_with(hex));
        match (from.next(), from.next()) {
            (Some((_, (_, Some(to)))), None) => out.push(PlanAnswerInput {
                item: format!("{TENSION_DISPLAY_PREFIX}{}", to.id),
                ..input.clone()
            }),
            // the tension matches two items: not carried, so the item stays open
            (Some((_, (_, None))), None) => {}
            _ => out.push(input.clone()),
        }
    }
    Ok((answers.into_values().collect(), out))
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
    // a tension item seals only as updated, confirmed or dismissed (D-ME2HD, D-45RG6): an open
    // one is an unanswered tension, so the version is not ready
    let unanswered = computed
        .items
        .iter()
        .filter(|c| c.kind == PlanKind::Tension && outcome_of(c, &answers) == PlanOutcome::Open)
        .count();
    if unanswered > 0 {
        let pref = crate::gate::resolve_in(st, &version.to_string())?;
        return Err(crate::gate::proposal_not_ready(
            &pref,
            vec![plan_open_issue(unanswered)],
        ));
    }
    let items: Vec<SealedItem> = computed
        .items
        .iter()
        .map(|c| (c, outcome_of(c, &answers)))
        .map(|(c, outcome)| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::E_PLAN_ITEM;
    use crate::gate::changes_of;
    use crate::{Rdy, Session, WriteRequest, WriteResult, parse_ops};
    use rdy_model::Role;
    use serde_json::{Value, json};

    fn writer() -> Session {
        Session::human("human:qa".into(), Role::Writer).unwrap()
    }

    fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
        r.write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
                justification: None,
                message: format!("write to {view}"),
            },
        )
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
    }

    /// View `a`: a deprecated doc with claim `x`, and a draft claim `y` citing it (one advisory
    /// tension of the version). Returns `x`.
    fn tension_of_a(r: &mut Rdy) -> String {
        let w = put(
            r,
            "a",
            json!([
                {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "old", "title": "Old", "doc_type": "overview", "lifecycle": "deprecated"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Old rules"}},
                {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X is the old rule."},
                {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Drafts"}}
            ]),
        );
        let (x, t) = (w.created["$x"].clone(), w.created["$t"].clone());
        put(
            r,
            "a",
            json!([{"op": "create", "type": "claim", "id": "$y", "parent": t, "body": format!("Y follows {{{{{x}}}}}.")}]),
        );
        x
    }

    fn tension_ids(p: &ProposalPlan) -> Vec<String> {
        p.summary
            .items
            .iter()
            .filter(|i| i.kind == PlanKind::Tension)
            .map(|i| i.id.clone())
            .collect()
    }

    /// Rulings task-5 F3 (O1): with rdy's key change written before the plan, an answer to the
    /// tension id the author saw before it moves to the item of the same tension, from the
    /// view's recorded answers and from the request, with its act and reason.
    #[test]
    fn rg5_a_tension_answer_carries_across_the_key_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let x = tension_of_a(&mut r);
        let by = writer().provenance();
        let st = r.read();
        let pre_changes = changes_of(&st, &st.included("a")).unwrap();
        let pre = prepare_proposal_plan(&st, &by, "a", None, Some(&pre_changes), &[]).unwrap();
        drop(st);
        let ids = tension_ids(&pre);
        assert_eq!(ids.len(), 1, "{:#?}", pre.summary);
        let tn = ids[0].clone();
        // the author answers it on the view
        let confirm = |reason: &str| {
            [PlanAnswerInput {
                item: tn.clone(),
                act: PlanAct::Confirm,
                reason: Some(reason.into()),
            }]
        };
        r.answer_plan(&writer(), "a", &confirm("x still guides y"))
            .unwrap();
        let before = r.read();

        // the key change: x, a side of the tension, gets a new key, as rdy's K1 writes it
        put(
            &mut r,
            "a",
            json!([{"op": "update", "id": x, "fields": {"key": "old.x-2"}}]),
        );
        let after = r.read();
        let changes = changes_of(&after, &after.included("a")).unwrap();
        // the tension has a new id now: the recorded answer no longer answers it, and the
        // earlier id names no item
        let plain = prepare_proposal_plan(&after, &by, "a", None, Some(&changes), &[]).unwrap();
        let post = tension_ids(&plain);
        assert_eq!(post.len(), 1, "{:#?}", plain.summary);
        assert_ne!(post[0], tn);
        let Err(e) =
            prepare_proposal_plan(&after, &by, "a", None, Some(&changes), &confirm("again"))
        else {
            panic!("the earlier id names an item");
        };
        assert_eq!(e.code, E_PLAN_ITEM, "{e}");
        let later = compute::tension_set(&after, "a", &changes).unwrap();
        assert_eq!(later.len(), 1);
        let item = compute::tension_candidate(&later[0], None).unwrap();

        // carried: the recorded answer, then the request's, each closes the later item
        let before_key = || BeforeKeyChange {
            st: &before,
            changes: &pre_changes,
        };
        for (inputs, reason) in [
            (vec![], "x still guides y"),
            (
                confirm("x guides y until it goes").to_vec(),
                "x guides y until it goes",
            ),
        ] {
            let pp = prepare_proposal_plan_keyed(
                &after,
                Some(before_key()),
                &by,
                "a",
                None,
                Some(&changes),
                &inputs,
            )
            .unwrap();
            assert!(tension_ids(&pp).is_empty(), "{:#?}", pp.summary);
            assert_eq!(
                serde_json::to_value(&pp.summary.by_kind["tension"]).unwrap(),
                json!({"open": 0, "closed": 1, "deferred": 0})
            );
            assert_eq!(pp.answers.len(), 1, "{:#?}", pp.answers);
            let a = &pp.answers[0];
            assert_eq!((a.item, a.cause), (item.id, later[0].vkey));
            assert_eq!(a.act, PlanAct::Confirm);
            assert_eq!(a.reason.as_deref(), Some(reason));
        }
    }
}
