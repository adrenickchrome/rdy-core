//! Readiness (D-ME2HD): what a proposed version must meet before an approval request is raised,
//! and the assessment record a ready version stores (D-SDSV2).
//!
//! # The one readiness function (D-ME2HD "Readiness, one pure function run at propose")
//!
//! [`readiness`] is the only readiness. The queue and every approval path run its checks on the
//! version as the queue reads it, once; no path keeps its own copy of one. A version that fails
//! is not ready (D-3JZP4): rdy lists each issue with its fix, raises no approval request (its
//! [`Readiness::card`] is none), and refuses an approval of it with `E_PROPOSAL_NOT_READY` and
//! these issues. A candidate's plan is the one the store binds to it, or a plan in hand that the
//! store does not hold yet ([`PlanSource`]); both give the same readiness.
//!
//! The issues come in this closed order:
//! 1. the landing's checks (`gate::landing_readiness`): a conflict on a block it writes, a
//!    blocking tension it would put into canon, an error in the canon it would give;
//! 2. check 1 ([`check`]): one `E_BRIEF_REQUIRED` issue for each change that touches stable
//!    content and records no briefing (O3);
//! 3. a version with no summary;
//! 4. a bound change plan that cannot be read, when nothing above failed;
//! 5. the change-plan gate's error (`E_PLAN_OPEN`: open non-code items on stable content and
//!    unanswered tension items, D-FQ7AK, D-45RG6); its `W_PLAN_OPEN` (drafts) is a warning,
//!    never an issue;
//! 6. a version whose reach exceeds `MAX_ROWS` (O9);
//! 7. the later refusals ([`later_refusals`]): a rejected decision, a rejected one_way
//!    re-affirmation, refused deferrals.
//!
//! Readiness never adds the issue of a version with no stored assessment (O10).

pub mod assessment;
pub mod check;

use crate::error::{E_VALIDATION, RdyError};
use crate::gate::ProposalRef;
use crate::pending::Item;
use crate::pending::queue::{answered, deferrals_refused};
use crate::plan::lifecycle::PlanSummary;
use crate::tensions::Tension;
use crate::{ReadState, Scope};
use rdy_model::assessment::MAX_ROWS;
use rdy_model::plan::Plan;
use rdy_model::{AnswerAct, BlockId, Change, Hash, Proposal};
use rdy_schema::{Issue, Severity};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Where a candidate's change plan comes from. A candidate is built once per readiness run, so
/// the plan in hand stays inline, as the interface gives it.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PlanSource {
    /// The plan the store binds to the version (`None`: a version with no bound plan, O19),
    /// read from the store.
    Bound(Option<Hash>),
    /// A plan the store does not hold yet (a propose binds it in its own batch), and its hash.
    InHand { plan: Plan, hash: Hash },
}

impl PlanSource {
    /// The bound plan's hash: none for a version with no bound plan.
    pub fn hash(&self) -> Option<Hash> {
        match self {
            PlanSource::Bound(h) => *h,
            PlanSource::InHand { hash, .. } => Some(*hash),
        }
    }
}

/// The version readiness reads: its ref, its record and its change plan.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub pref: ProposalRef,
    pub p: Proposal,
    pub plan: PlanSource,
}

impl Candidate {
    /// Version `pref` (record `p`) with the plan the store binds to it.
    pub fn stored(st: &ReadState, pref: ProposalRef, p: Proposal) -> Result<Candidate, RdyError> {
        let bound = st.reader().plan_of_proposal(&pref.version, st.seq())?;
        Ok(Candidate {
            pref,
            p,
            plan: PlanSource::Bound(bound),
        })
    }
}

/// What [`readiness`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readiness {
    /// Each failed check, one issue each, in the closed order (module docs). Empty: ready.
    pub issues: Vec<Issue>,
    /// The version's change plan with its bound answers, for the assessment and the reply. The
    /// default when the plan cannot be read.
    pub summary: PlanSummary,
    /// The version's `approve` item as the queue builds it, its card not rendered yet, when the
    /// version is ready; none when it is not.
    pub card: Option<Item>,
}

/// The readiness of candidate `c` on `st` (see the module docs). It writes nothing.
pub fn readiness(st: &ReadState, c: &Candidate) -> Result<Readiness, RdyError> {
    crate::pending::queue::readiness_of(st, c)
}

/// What the checks read of a version: the queue's version read reads it once.
pub(crate) struct Read<'a> {
    pub(crate) pref: &'a ProposalRef,
    pub(crate) p: &'a Proposal,
    pub(crate) changes: &'a [Change],
    /// The version's bound plan: none for a version with no bound plan (O19).
    pub(crate) plan_bound: Option<Hash>,
    /// Its change plan, or the error of reading it.
    pub(crate) plan: Result<&'a PlanSummary, &'a RdyError>,
    /// The blocks its plan items name: each subject that is a block, and each cause (O18).
    pub(crate) plan_blocks: &'a BTreeSet<BlockId>,
    /// The blocks its changes write.
    pub(crate) written: &'a BTreeSet<BlockId>,
    /// Its open tensions.
    pub(crate) tensions: &'a [Arc<Tension>],
    /// (id, `<id>@<short version>`) of each proposed decision.
    pub(crate) decisions: Vec<(BlockId, String)>,
    /// (id, ref, one_way) of each re-affirmed decision.
    pub(crate) reaffirms: Vec<(BlockId, String, bool)>,
}

/// The checks of readiness over what `r` read, in the closed order (module docs): the issues
/// of [`readiness`].
pub(crate) fn issues(st: &ReadState, r: &Read<'_>) -> Result<Vec<Issue>, RdyError> {
    READINESS_CALLS.with(|n| n.set(n.get() + 1));
    // 1. the landing's checks
    let mut issues = crate::gate::landing_readiness(st, r.pref, r.p, r.changes)?;
    // 2. check 1: a stable change records a briefing (O3)
    issues.extend(check::briefing_issues(st, r.p, r.changes)?);
    // 3. the summary
    let summary = r.p.summary.as_deref().map(str::trim);
    if summary.is_none_or(str::is_empty) {
        issues.push(Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: None,
            message: format!("{} has no summary", r.pref),
            fix: "propose it again with a summary in plain words for the person who approves it"
                .into(),
        });
    }
    // 4. a bound version whose change plan cannot be read cannot show its deferrals; a version
    // that fails another check (a conflict, say) says that first
    if issues.is_empty()
        && r.plan_bound.is_some()
        && let Err(e) = &r.plan
    {
        issues.push(Issue {
            severity: Severity::Error,
            code: e.code,
            block: None,
            message: e.message.clone(),
            fix: e.fix.clone(),
        });
    }
    // 5. the change-plan gate on stable content and tension items; its warning is no issue
    if let Ok(summary) = &r.plan {
        issues.extend(
            crate::plan::lifecycle::plan_issues(summary)
                .into_iter()
                .filter(|i| i.severity == Severity::Error),
        );
    }
    // 6. the reach bound (O9, O18): what the version writes, what its plan items name, and the
    // blocks of its tensions
    let mut reach: BTreeSet<&BlockId> = r.written.iter().chain(r.plan_blocks).collect();
    for t in r.tensions {
        reach.extend(t.involved.iter().map(|i| &i.block));
    }
    if reach.len() > MAX_ROWS {
        issues.push(assessment::reach_too_large(r.pref));
    }
    // 7. the later refusals
    issues.extend(later_refusals(
        st,
        r.pref,
        &r.p.view,
        &r.decisions,
        &r.reaffirms,
        r.plan_bound.is_some(),
    )?);
    Ok(issues)
}

/// The later refusals (D-ME2HD "Later refusals"), from inputs that need no candidate build:
/// `decisions` = (id, `<id>@<short version>`) of each proposed decision; `reaffirms` = (id, ref,
/// one_way) of each re-affirmed decision. One issue for each proposed decision whose current
/// version has a `reject_decision` answer (F1), exception or not; then one for each one_way
/// re-affirmation the person rejected (D-GV465); then the refused deferrals of a version with a
/// bound plan (`bound`, D-AAQ9Z), never an acceptance. The fix names `view`, the version's view.
pub fn later_refusals(
    st: &ReadState,
    pref: &ProposalRef,
    view: &str,
    decisions: &[(BlockId, String)],
    reaffirms: &[(BlockId, String, bool)],
    bound: bool,
) -> Result<Vec<Issue>, RdyError> {
    let mut issues = vec![];
    for (id, r) in decisions {
        if answered(st, r, AnswerAct::RejectDecision)? {
            issues.push(Issue {
                severity: Severity::Error,
                code: E_VALIDATION,
                block: Some(id.clone()),
                message: format!("{id} was rejected; drop or revise it, then propose again"),
                fix: format!("in view {view}, drop or revise {id}, then propose again"),
            });
        }
    }
    for (id, r, _) in reaffirms.iter().filter(|(_, _, one_way)| *one_way) {
        if answered(st, r, AnswerAct::RejectReaffirm)? {
            issues.push(Issue {
                severity: Severity::Error,
                code: E_VALIDATION,
                block: Some(id.clone()),
                message: format!("{id}'s re-affirmation was rejected"),
                fix: "drop the re-affirmation and propose again".into(),
            });
        }
    }
    if bound {
        issues.extend(deferrals_refused(st, pref)?);
    }
    Ok(issues)
}

/// The scope of the canon a version recorded (`canon_at`): `None` for the empty canon; canon's
/// head reads as canon.
pub(crate) fn canon_scope(st: &ReadState, canon_at: Option<Hash>) -> Option<Scope> {
    let h = canon_at?;
    Some(if Some(h) == st.canon_head() {
        Scope::Canon
    } else {
        Scope::At(h)
    })
}

thread_local! {
    /// This thread's runs of the readiness checks (see `readiness_calls_for_tests`).
    static READINESS_CALLS: Cell<u64> = const { Cell::new(0) };
    /// This thread's candidate builds of the queue's version read.
    static CANDIDATE_BUILDS: Cell<u64> = const { Cell::new(0) };
}

/// Counts one candidate build of the queue's version read.
pub(crate) fn count_candidate_build() {
    CANDIDATE_BUILDS.with(|n| n.set(n.get() + 1));
}

/// Tests only (as `plan_ref_reads_for_tests`): this thread's calls of [`readiness`], counted on
/// every path that runs its checks (the queue and the approval paths too).
#[doc(hidden)]
pub fn readiness_calls_for_tests() -> u64 {
    READINESS_CALLS.with(Cell::get)
}

/// Tests only: this thread's candidate builds of the queue's version read (the prospective
/// canon of a version, `coverage::change::candidate_build`).
#[doc(hidden)]
pub fn candidate_builds_for_tests() -> u64 {
    CANDIDATE_BUILDS.with(Cell::get)
}
