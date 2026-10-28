//! The assessment snapshot: what a version's readiness analysis found, stored as a pure value.
//!
//! The snapshot copies no block body, field map or live graph. It keeps ids, hashes, counts and
//! the computed texts a reader needs. The declaration order of every struct and enum is the wire
//! format (postcard's field order and variant index), and the snake_case names are the editable
//! JSON and YAML forms. Unknown keys are refused in every struct.
//!
//! [`Assessment::validate`] checks the structure of a decoded record and never normalizes it.
//! It reports the first failed check, in this order: method, pack, policy, facts, tensions,
//! suspects, coverage, confidence, reach, card, scope, issues. For each list it checks the size
//! limit, then the sort order and distinctness, then each row's fields in their declared order.

use crate::{BlockId, Hash};
use serde::{Deserialize, Serialize};

/// The most rows any stored list holds.
pub const MAX_ROWS: usize = 100_000;
/// The most bytes in one card fact, issue fix or tension summary.
pub const MAX_TEXT_BYTES: usize = 4_096;
/// The most bytes in a pack name or version.
pub const MAX_PACK_BYTES: usize = 256;
/// The most bytes in a policy rule or role name.
pub const MAX_POLICY_NAME_BYTES: usize = 256;
/// The most reasons a verdict that needs a person carries.
pub const MAX_POLICY_REASONS: usize = 100;
/// The most bytes in one policy reason.
pub const MAX_POLICY_REASON_BYTES: usize = 4_096;
/// The highest score.
pub const MAX_SCORE: u32 = 100;

/// The dimension names an issue may carry.
pub const DIMENSIONS: [&str; 6] = [
    "grounded",
    "consistent",
    "current",
    "connected",
    "complete",
    "governed",
];
/// The issue codes.
pub const ISSUE_CODES: [&str; 12] = [
    "untied_content",
    "unlinked_mention",
    "open_tension",
    "dismissed_tension",
    "open_suspect",
    "unlinked_doc",
    "dangling_ref",
    "stub_section",
    "missing_section",
    "open_question",
    "ungrounded",
    "unapplied_decision",
];
/// The tension kinds a stored tension row may carry.
pub const TENSION_KINDS: [&str; 10] = [
    "edit_collision",
    "fork",
    "premise_shift",
    "area_overlap",
    "duplicate_definition",
    "unjustified_change",
    "deprecated_reference",
    "value_disagreement",
    "restatement_drift",
    "semantic",
];
/// The plan item kinds a count row may name.
pub const PLAN_KINDS: [&str; 6] = [
    "update",
    "rejustify",
    "reaffirm",
    "confirm",
    "update_implementation",
    "tension",
];

const SCOPE: &str = "invalid assessment scope";
const PACK: &str = "invalid assessment pack";
const METHOD: &str = "invalid assessment method";
const VOCABULARY: &str = "invalid assessment vocabulary";
const SCORE: &str = "invalid assessment score";
const POLICY: &str = "invalid assessment policy";
const ORDER: &str = "assessment rows are not sorted and distinct";
const LIMIT: &str = "assessment text exceeds its limit";

/// A policy block and the version of it that applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRef {
    pub block: BlockId,
    pub version: Hash,
}

/// What the authority policy said.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyVerdict {
    Accepts {
        rule: String,
    },
    NeedsPerson {
        role: Option<String>,
        reasons: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessedPolicy {
    pub policy: Option<PolicyRef>,
    pub verdict: PolicyVerdict,
}

/// The rule pack that produced the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentPack {
    pub name: String,
    pub version: String,
    pub digest: Hash,
}

/// The population the analysis covered. `proposal` equals the outer proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentScope {
    pub proposal: Hash,
    pub blocks: Vec<BlockId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentLevel {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentSeverity {
    Advisory,
    Blocking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentTensionStatus {
    Open,
    Dismissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentReversibility {
    OneWay,
    TwoWay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentExceptionKind {
    Against,
    OneWay,
    Unapplied,
    NotRecommended,
    BlockingTension,
    ReaffirmOneWay,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentException {
    pub kind: AssessmentExceptionKind,
    pub reference: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentReversal {
    pub decision: BlockId,
    pub value: AssessmentReversibility,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentCounts {
    pub open: u64,
    pub closed: u64,
    pub deferred: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentPlanCounts {
    pub open: u64,
    pub closed: u64,
    pub deferred: u64,
    pub by_kind: Vec<(String, AssessmentCounts)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentFacts {
    pub changes: u64,
    pub consequences: u64,
    pub plan: AssessmentPlanCounts,
    pub supersedes: Vec<BlockId>,
    pub reaffirms: Vec<BlockId>,
    pub reversibility: Vec<AssessmentReversal>,
    pub exceptions: Vec<AssessmentException>,
}

/// The plan object and item hash a tension's recorded answer names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentPlanAnswerRef {
    pub plan: Hash,
    pub item: Hash,
}

/// One tension row: its computed summary, the blocks it involves and its content-bound key. It
/// copies no block body. `dismissal` names a REVIEW object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentTension {
    pub id: Hash,
    pub kind: String,
    pub severity: AssessmentSeverity,
    pub status: AssessmentTensionStatus,
    pub answer: Option<AssessmentPlanAnswerRef>,
    pub dismissal: Option<Hash>,
    pub summary: String,
    pub blocks: Vec<BlockId>,
    pub vkey: Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentSuspect {
    pub block: BlockId,
    pub cause: BlockId,
}

/// Seven scores, each an integer from 0 to 100.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentScores {
    pub overall: u32,
    pub grounded: u32,
    pub consistent: u32,
    pub current: u32,
    pub connected: u32,
    pub complete: u32,
    pub governed: u32,
}

impl AssessmentScores {
    fn in_range(&self) -> bool {
        [
            self.overall,
            self.grounded,
            self.consistent,
            self.current,
            self.connected,
            self.complete,
            self.governed,
        ]
        .into_iter()
        .all(|s| s <= MAX_SCORE)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentScoreChange {
    pub before: AssessmentScores,
    pub after: AssessmentScores,
}

/// A page's scores before and after the version. A page that did not exist before has seven
/// before scores of 100, as the coverage rule has it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentPage {
    pub doc: BlockId,
    pub before: AssessmentScores,
    pub after: AssessmentScores,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentCoverage {
    pub pages: Vec<AssessmentPage>,
    pub corpus: AssessmentScoreChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentIssue {
    pub block: BlockId,
    pub dimension: String,
    pub code: String,
    pub subject: Option<String>,
    pub fix: String,
    pub dismissal: Option<Hash>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentConfidence {
    pub level: AssessmentLevel,
    pub reasons: Vec<AssessmentIssue>,
}

/// The facts of the card the person saw, in presentation order. Duplicate text is allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentCard {
    pub hash: Hash,
    pub facts: Vec<String>,
}

/// A version's stored assessment. `reach` is the blocks the version writes, the blocks its
/// consequences and plan items name, and the sides of the tensions that involve it. It leaves
/// out blocks read only for coverage, so it differs from `scope.blocks`. The two omission counts
/// say how many rows the capped lists left out and accept every u64 value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    pub proposal: Hash,
    pub base: Option<Hash>,
    pub method: u32,
    pub pack: AssessmentPack,
    pub config: Hash,
    pub policy: Option<AssessedPolicy>,
    pub facts: AssessmentFacts,
    pub tensions: Vec<AssessmentTension>,
    pub tensions_omitted: u64,
    pub suspects: Vec<AssessmentSuspect>,
    pub suspects_omitted: u64,
    pub coverage: AssessmentCoverage,
    pub confidence: AssessmentConfidence,
    pub reach: Vec<BlockId>,
    pub card: AssessmentCard,
    pub scope: AssessmentScope,
    pub issues: Vec<AssessmentIssue>,
}

/// Whether `rows` rise strictly by `key`: sorted, with no two rows equal.
pub(crate) fn strictly_increasing<'a, T, K: Ord>(rows: &'a [T], key: impl Fn(&'a T) -> K) -> bool {
    rows.windows(2).all(|w| key(&w[0]) < key(&w[1]))
}

/// A byte length in `min..=max`.
fn bytes_between(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
}

fn ids_sorted(ids: &[BlockId]) -> Result<(), &'static str> {
    if ids.len() > MAX_ROWS {
        return Err(LIMIT);
    }
    if !strictly_increasing(ids, |b| b) {
        return Err(ORDER);
    }
    Ok(())
}

/// The sort key of an issue row.
type IssueKey<'a> = (
    &'a BlockId,
    &'a str,
    &'a str,
    &'a Option<String>,
    &'a str,
    &'a Option<Hash>,
);

fn issue_key(i: &AssessmentIssue) -> IssueKey<'_> {
    (
        &i.block,
        i.dimension.as_str(),
        i.code.as_str(),
        &i.subject,
        i.fix.as_str(),
        &i.dismissal,
    )
}

fn issues_valid(rows: &[AssessmentIssue]) -> Result<(), &'static str> {
    if rows.len() > MAX_ROWS {
        return Err(LIMIT);
    }
    if !strictly_increasing(rows, issue_key) {
        return Err(ORDER);
    }
    for i in rows {
        if !DIMENSIONS.contains(&i.dimension.as_str()) || !ISSUE_CODES.contains(&i.code.as_str()) {
            return Err(VOCABULARY);
        }
        if i.fix.len() > MAX_TEXT_BYTES {
            return Err(LIMIT);
        }
    }
    Ok(())
}

fn scores_valid(rows: &[&AssessmentScores]) -> Result<(), &'static str> {
    if rows.iter().all(|s| s.in_range()) {
        Ok(())
    } else {
        Err(SCORE)
    }
}

impl PolicyVerdict {
    fn validate(&self) -> Result<(), &'static str> {
        match self {
            PolicyVerdict::Accepts { rule } => {
                if !bytes_between(rule, 1, MAX_POLICY_NAME_BYTES) {
                    return Err(POLICY);
                }
            }
            PolicyVerdict::NeedsPerson { role, reasons } => {
                if let Some(role) = role
                    && !bytes_between(role, 1, MAX_POLICY_NAME_BYTES)
                {
                    return Err(POLICY);
                }
                if !(1..=MAX_POLICY_REASONS).contains(&reasons.len())
                    || reasons
                        .iter()
                        .any(|r| !bytes_between(r, 1, MAX_POLICY_REASON_BYTES))
                {
                    return Err(POLICY);
                }
            }
        }
        Ok(())
    }
}

impl AssessedPolicy {
    /// A missing policy is valid only with a verdict that needs a person.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.policy.is_none() && !matches!(self.verdict, PolicyVerdict::NeedsPerson { .. }) {
            return Err(POLICY);
        }
        self.verdict.validate()
    }
}

impl AssessmentFacts {
    fn validate(&self) -> Result<(), &'static str> {
        let plan = &self.plan;
        if plan.by_kind.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if !strictly_increasing(&plan.by_kind, |(name, _)| name.as_str()) {
            return Err(ORDER);
        }
        if plan
            .by_kind
            .iter()
            .any(|(name, _)| !PLAN_KINDS.contains(&name.as_str()))
        {
            return Err(VOCABULARY);
        }
        ids_sorted(&self.supersedes)?;
        ids_sorted(&self.reaffirms)?;
        if self.reversibility.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if !strictly_increasing(&self.reversibility, |r| &r.decision) {
            return Err(ORDER);
        }
        if self.exceptions.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if !strictly_increasing(&self.exceptions, |e| (e.kind, e.reference.as_str())) {
            return Err(ORDER);
        }
        Ok(())
    }
}

impl Assessment {
    /// Checks the structure of a decoded snapshot. It never normalizes the record.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.method == 0 {
            return Err(METHOD);
        }
        if !bytes_between(&self.pack.name, 1, MAX_PACK_BYTES)
            || !bytes_between(&self.pack.version, 1, MAX_PACK_BYTES)
        {
            return Err(PACK);
        }
        if let Some(policy) = &self.policy {
            policy.validate()?;
        }
        self.facts.validate()?;
        self.validate_tensions()?;
        if self.suspects.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if !strictly_increasing(&self.suspects, |s| (&s.block, &s.cause)) {
            return Err(ORDER);
        }
        self.validate_coverage()?;
        issues_valid(&self.confidence.reasons)?;
        ids_sorted(&self.reach)?;
        if self.card.facts.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if self.card.facts.iter().any(|f| f.len() > MAX_TEXT_BYTES) {
            return Err(LIMIT);
        }
        if self.scope.proposal != self.proposal {
            return Err(SCOPE);
        }
        ids_sorted(&self.scope.blocks)?;
        issues_valid(&self.issues)
    }

    fn validate_tensions(&self) -> Result<(), &'static str> {
        if self.tensions.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if !strictly_increasing(&self.tensions, |t| t.id) {
            return Err(ORDER);
        }
        for t in &self.tensions {
            if !TENSION_KINDS.contains(&t.kind.as_str()) {
                return Err(VOCABULARY);
            }
            if t.summary.len() > MAX_TEXT_BYTES {
                return Err(LIMIT);
            }
            ids_sorted(&t.blocks)?;
        }
        Ok(())
    }

    fn validate_coverage(&self) -> Result<(), &'static str> {
        let pages = &self.coverage.pages;
        if pages.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        if !strictly_increasing(pages, |p| &p.doc) {
            return Err(ORDER);
        }
        for p in pages {
            scores_valid(&[&p.before, &p.after])?;
        }
        let corpus = &self.coverage.corpus;
        scores_valid(&[&corpus.before, &corpus.after])
    }
}
