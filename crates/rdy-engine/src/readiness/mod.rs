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
//! 3. a version with no summary; then the rest of D-ME2HD's item 6 (D-JQD5X): the landing
//!    projection's refusal (a choice that names no decision for its considered options,
//!    `E_ALT_DECISION`, or a view that moved on), and one `E_RULE_CHECK` issue for each blocking
//!    pack rule that fails on stable content of the projected canon (the first 256);
//! 4. a bound change plan that cannot be read, when nothing above failed;
//! 5. the change-plan gate's error (`E_PLAN_OPEN`: open non-code items on stable content and
//!    unanswered tension items, D-FQ7AK, D-45RG6); its `W_PLAN_OPEN` (drafts) is a warning,
//!    never an issue;
//! 6. a version whose reach exceeds `MAX_ROWS` (O9), or whose rules widen it past that bound
//!    ("too large to assess", D-DSPMW);
//! 7. the later refusals ([`later_refusals`]): a rejected decision, a rejected one_way
//!    re-affirmation, refused deferrals. The landing adds a fourth, which only an assessment
//!    can date: an advice accepted after it ([`accepted_advice`], ruling task-12 R1);
//! 8. the policy change rule (O19, D-ZWP7Q): one `E_VALIDATION` issue for each
//!    `authority_policy` block the version creates, updates or deletes whose last change in the
//!    version does not implement a decision of the prospective canon;
//! 9. the coverage gate (O17, D-ZWP7Q, D-WB22B, D-T5S9A): with `coverage.gate: no_lower`, one
//!    `E_COVERAGE_LOWERED` issue for each page the version lowers, in doc id order, then one
//!    for the corpus.
//!
//! A ready version also gets the authority policy's verdict ([`Readiness::policy`],
//! `policy_verdict::verdict`), under the live policy: the policy block in canon at its head,
//! never the candidate's or a view's.
//!
//! Readiness never adds the issue of a version with no stored assessment (O10).

pub mod assessment;
pub mod check;

use crate::error::{E_VALIDATION, RdyError};
use crate::gate::ProposalRef;
use crate::pending::Item;
use crate::pending::queue::{answered, deferrals_refused};
use crate::plan::lifecycle::PlanSummary;
use crate::{ReadState, Scope};
use assessment::MAX_TENSION_ROWS;
use rdy_model::assessment::Assessment;
use rdy_model::assessment::{AssessedPolicy, PolicyVerdict};
use rdy_model::plan::Plan;
use rdy_model::{AnswerAct, BlockId, Change, Hash, Proposal};
use rdy_schema::{Issue, Severity};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::BTreeSet;

/// Where a candidate's change plan comes from. A candidate is built once per readiness run, so
/// the plan in hand stays inline, as the interface gives it.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PlanSource {
    /// The plan the store binds to the version (`None`: a version with no bound plan, O19),
    /// read from the store.
    Bound(Option<Hash>),
    /// A plan the store does not hold yet (a propose binds it in its own batch), and its hash.
    /// `computed`: the change plan the propose computed for the version, which the plan read
    /// uses on the same state with the same inputs (none: the read computes it).
    InHand {
        plan: Plan,
        hash: Hash,
        computed: Option<crate::plan::compute::CarriedPlan>,
    },
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
    /// The authority policy's verdict on the version (D-ZWP7Q), when it is ready; none when it
    /// is not.
    pub policy: Option<AssessedPolicy>,
    /// The `W_RULE_CHECK` warnings of the version's pack rules (D-JQD5X): the first 256, in rule
    /// then block order. Never an issue.
    pub warnings: Vec<Issue>,
    /// The rule rows left out past the cut: (blocking, advisory).
    pub checks_omitted: (usize, usize),
}

/// `rdy_propose`'s `policy` (O13): the verdict a ready version's assessment stores, as the reply
/// gives it. `block` and `version` are the live policy's (none with no policy); `rule` names the
/// accepting rule; `role` the people the version needs; `reasons` why it needs them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalPolicy {
    pub block: Option<String>,
    pub version: Option<String>,
    /// `accepts` or `needs_person`.
    pub verdict: String,
    pub rule: Option<String>,
    pub role: Option<String>,
    pub reasons: Vec<String>,
}

/// The verdict word of an accepting verdict.
pub const ACCEPTS: &str = "accepts";
/// The verdict word of a verdict that needs a person.
pub const NEEDS_PERSON: &str = "needs_person";

impl From<&AssessedPolicy> for ProposalPolicy {
    fn from(a: &AssessedPolicy) -> ProposalPolicy {
        let (verdict, rule, role, reasons) = match &a.verdict {
            PolicyVerdict::Accepts { rule } => (ACCEPTS, Some(rule.clone()), None, vec![]),
            PolicyVerdict::NeedsPerson { role, reasons } => {
                (NEEDS_PERSON, None, role.clone(), reasons.clone())
            }
        };
        ProposalPolicy {
            block: a.policy.as_ref().map(|p| p.block.to_string()),
            version: a.policy.as_ref().map(|p| p.version.to_string()),
            verdict: verdict.to_string(),
            rule,
            role,
            reasons,
        }
    }
}

impl ProposalPolicy {
    /// Whether the policy accepts the version.
    pub fn accepts(&self) -> bool {
        self.verdict == ACCEPTS
    }

    /// The card's `Policy` fact value (O9), as `policy_verdict::fact_value` gives it from the
    /// stored verdict. None when the reply's block or version does not read.
    pub fn fact_value(&self) -> Option<String> {
        let policy = match (&self.block, &self.version) {
            (Some(b), Some(v)) => Some(rdy_model::assessment::PolicyRef {
                block: BlockId::parse(b).ok()?,
                version: v.parse().ok()?,
            }),
            (None, None) => None,
            _ => return None,
        };
        let verdict = match self.verdict.as_str() {
            ACCEPTS => PolicyVerdict::Accepts {
                rule: self.rule.clone()?,
            },
            NEEDS_PERSON => PolicyVerdict::NeedsPerson {
                role: self.role.clone(),
                reasons: self.reasons.clone(),
            },
            _ => return None,
        };
        Some(crate::policy_verdict::fact_value(&AssessedPolicy {
            policy,
            verdict,
        }))
    }
}

/// The readiness of candidate `c` on `st` (see the module docs). It writes nothing.
pub fn readiness(st: &ReadState, c: &Candidate) -> Result<Readiness, RdyError> {
    crate::pending::queue::readiness_of(st, c)
}

/// What [`assess`] found: the readiness of a candidate and, when it is ready, the assessment
/// record it stores (D-SDSV2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessed {
    pub readiness: Readiness,
    /// None exactly when `readiness` has an issue.
    pub assessment: Option<Assessment>,
}

/// [`readiness`] of candidate `c` on `st`, and for a ready candidate the assessment record
/// ([`assessment::build`]) from the same read of the version, with the gate configuration
/// `gate` (O7, O18): what a propose stores in the version's batch (D-ME2HD). A record
/// the store would refuse (O9) makes the candidate not ready, with that one issue. It writes
/// nothing.
pub fn assess(
    st: &ReadState,
    c: &Candidate,
    gate: &assessment::GateConfig,
) -> Result<Assessed, RdyError> {
    let (readiness, assessment) = crate::pending::queue::assessed(st, c, gate)?;
    Ok(Assessed {
        readiness,
        assessment,
    })
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
    /// (id, `<id>@<short version>`) of each proposed decision.
    pub(crate) decisions: Vec<(BlockId, String)>,
    /// (id, ref, one_way) of each re-affirmed decision.
    pub(crate) reaffirms: Vec<(BlockId, String, bool)>,
    /// O19: each `authority_policy` block the version creates, updates or deletes, in id order,
    /// with the short hash of the last change of the version that writes it and whether that
    /// change implements a decision of the prospective canon.
    pub(crate) policy_changes: &'a [PolicyChange],
    /// D-ME2HD item 6 (D-JQD5X): the landing projection's refusal and the blocking rule issues.
    pub(crate) item6: &'a [Issue],
    /// The reach the version's rules add (D-DSPMW); Err: the "too large to assess" issue.
    pub(crate) rule_reach: Result<&'a BTreeSet<BlockId>, &'a Issue>,
}

/// O19: an `authority_policy` block a version writes, and the last change that writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PolicyChange {
    pub(crate) block: BlockId,
    /// The short hash of the last change of the version that writes `block`.
    pub(crate) short: String,
    /// Whether that change is `implements` with at least one decision of the prospective canon.
    pub(crate) implements: bool,
}

/// The checks of readiness over what `r` read, in the closed order (module docs): the issues
/// of [`readiness`]. `lowered` gives check 9's issues; it runs only under `coverage.gate:
/// no_lower`.
pub(crate) fn issues(
    st: &ReadState,
    r: &Read<'_>,
    lowered: impl FnOnce() -> Vec<Issue>,
) -> Result<Vec<Issue>, RdyError> {
    READINESS_CALLS.with(|n| n.set(n.get() + 1));
    // 1. the landing's checks
    let (mut issues, conflicted) = crate::gate::landing_readiness(st, r.pref, r.p, r.changes)?;
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
    // item 6's rest: the landing projection's refusal and the blocking rule checks
    issues.extend(r.item6.iter().cloned());
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
    // 6. the reach bound (O9, O18): the assessment's own reach
    if let Err(issue) = reach(st, r, &conflicted)? {
        issues.push(issue);
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
    // 8. the policy change rule (O19)
    issues.extend(policy_change_issues(r.policy_changes));
    // 9. the coverage gate (O17)
    if st.coverage_gate() == crate::CoverageGate::NoLower {
        issues.extend(lowered());
    }
    Ok(issues)
}

/// O19 (D-ZWP7Q "A change to it needs `implements` a decision"): one issue for each policy block
/// whose last change in the version implements no decision of the prospective canon. A later
/// write cannot change an earlier change's justification, so only the last change counts.
pub(crate) fn policy_change_issues(changes: &[PolicyChange]) -> Vec<Issue> {
    changes
        .iter()
        .filter(|c| !c.implements)
        .map(|c| Issue {
            severity: Severity::Error,
            code: E_VALIDATION,
            block: Some(c.block.clone()),
            message: format!(
                "change {} changes the authority policy and implements no decision",
                c.short
            ),
            fix: "write it again with implements: the decision that changes the policy".into(),
        })
        .collect()
}

/// O17: the `E_COVERAGE_LOWERED` issue of version `pref` lowering `what` (a doc path, or `the
/// corpus`) from `before` to `after` percent; `block` the page's doc, none for the corpus.
pub(crate) fn coverage_lowered(
    pref: &ProposalRef,
    block: Option<BlockId>,
    what: &str,
    before: u32,
    after: u32,
) -> Issue {
    Issue {
        severity: Severity::Error,
        code: crate::error::E_COVERAGE_LOWERED,
        block,
        message: format!("{pref} lowers coverage of {what} from {before}% to {after}%"),
        fix: crate::error::COVERAGE_LOWERED_FIX.into(),
    }
}

/// The reach of what `r` read (O18), as [`assessment::build`] computes it
/// ([`assessment::reach`]): with the blocks of the first [`MAX_TENSION_ROWS`] tension rows the
/// version lists, open and dismissed (O17: `ReadState::proposal_tensions` with max 1,000), less
/// those on blocks that all carry one of its blocking conflicts (`conflicted`). The rows are the
/// ones the version was made with (ruling task-8 F1). Err in the inner result: the O9 issue.
pub(crate) fn reach(
    st: &ReadState,
    r: &Read<'_>,
    conflicted: &BTreeSet<BlockId>,
) -> Result<Result<BTreeSet<BlockId>, Issue>, RdyError> {
    let fixed = crate::plan::compute::fixed_tensions(st, &r.pref.version)?;
    let rows = st.listed_tensions(
        &r.p.view,
        &r.p.changes,
        conflicted,
        fixed.as_ref().map(|f| &f.ids),
    )?;
    Ok(assessment::reach(
        r.pref,
        r.written,
        r.plan_blocks,
        rows.iter()
            .take(MAX_TENSION_ROWS)
            .flat_map(|(_, t)| t.blocks()),
        r.rule_reach,
    ))
}

/// The derived sets candidate `c`'s pack rules read in its landing projection (D-JQD5X): its
/// suspects, the blocks of its open tensions and each block's intrinsic coverage score.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleInputs {
    pub suspects: BTreeSet<BlockId>,
    pub tensions: BTreeSet<BlockId>,
    pub coverage: std::collections::BTreeMap<BlockId, u32>,
}

/// Tests only: every derived set the pack rules of candidate `c` can read, as readiness builds
/// them.
#[doc(hidden)]
pub fn rule_inputs_for_tests(st: &ReadState, c: &Candidate) -> Result<RuleInputs, RdyError> {
    crate::pending::queue::rule_inputs_of(st, c)
}

/// Tests only: the reach readiness's check 6 reads for candidate `c` (see `reach`). A reach over
/// the bound is an error.
#[doc(hidden)]
pub fn reach_for_tests(st: &ReadState, c: &Candidate) -> Result<BTreeSet<BlockId>, RdyError> {
    crate::pending::queue::reach_of(st, c)
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

/// Ruling task-12 R1 (D-WMCFW, D-ME2HD, D-JQD5X): the later refusal of an accepted advice. A
/// person's acceptance (`accept_advice`) makes a semantic tension block until a side's content
/// changes. One written after assessment `a` of version `pref` (record `p`) refuses its approval:
/// one `E_BLOCKING_TENSION` issue per accepted tension, with the landing check's issue text and
/// fix. It reads the semantic tension versions alone, with no candidate: an accepted pair is one
/// whose open record blocks and whose advisory form the assessment lists as an open advisory
/// tension (before the acceptance, the version was ready with it advisory). Empty when nothing
/// was accepted since.
pub fn accepted_advice(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
    a: &Assessment,
) -> Result<Vec<Issue>, RdyError> {
    use crate::tensions::info::TensionStatus;
    use crate::tensions::{Involved, SideRef, TensionKind, TensionSeverity, tension_vkey};
    use rdy_model::assessment::{AssessmentSeverity, AssessmentTensionStatus};
    let sem = st.semantic();
    if !sem.has_tensions() {
        return Ok(vec![]);
    }
    // the advisory vkeys the assessment lists open: a pair accepted after it was assessed
    let advisory: BTreeSet<Hash> = a
        .tensions
        .iter()
        .filter(|t| {
            t.kind == TensionKind::Semantic.name()
                && t.severity == AssessmentSeverity::Advisory
                && t.status == AssessmentTensionStatus::Open
        })
        .map(|t| t.vkey)
        .collect();
    let set = st.tensions()?;
    let mut cache = crate::tensions::info::InfoCache::default();
    let mut refusing = vec![];
    for o in sem.tensions().filter(|o| o.record.blocking) {
        let Some(item) = o.record.items.first() else {
            continue;
        };
        let forms: Vec<_> = set
            .involving_block(&item.block)
            .into_iter()
            .filter(|t| {
                crate::tensions::semantic::pair_of(t) == Some(o.record.key)
                    && t.severity == TensionSeverity::Blocking
            })
            .collect();
        // the version's own landing form, open: what check 3 refuses. A ready assessment had
        // none (check 3), so one now was accepted after the assessment, whether the assessment
        // listed the pair (advisory) or a verdict opened it later (ruling task-12 R1: "any
        // tension of the version, written after its assessment")
        let own = forms.iter().find(|t| {
            st.tension_status(t) == TensionStatus::Open
                && t.sides
                    .iter()
                    .any(|s| matches!(s, SideRef::Change(c) if p.changes.contains(c)))
                && t.sides.iter().all(|s| match s {
                    SideRef::Canon { .. } => true,
                    SideRef::Change(c) => p.changes.contains(c),
                    SideRef::Intent { .. } => false,
                })
        });
        let listed = || {
            let involved: Vec<Involved> = o
                .record
                .items
                .iter()
                .map(|i| Involved {
                    side: SideRef::Canon { landing: None },
                    block: i.block.clone(),
                    version: None,
                    content: Some(i.content),
                })
                .collect();
            advisory.contains(&tension_vkey(
                TensionKind::Semantic,
                TensionSeverity::Advisory,
                &involved,
            ))
        };
        let form = match own {
            Some(t) => Some(t),
            None if listed() => forms.first(),
            None => None,
        };
        if let Some(t) = form {
            refusing.push(st.tension_info_with(t, &mut cache));
        }
    }
    Ok(crate::gate::blocking_tensions(pref, &refusing).issues)
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
