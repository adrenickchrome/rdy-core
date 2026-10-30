//! The assessment record a ready version stores (D-SDSV2, D-ME2HD "The assessment").
//!
//! [`build`] is pure: it reads no store and no clock. Its caller computes every input (at
//! propose, and for the fresh card of a landing) and `build` puts them in the record's order:
//! each list sorted and distinct as [`Assessment::validate`] needs, the tension and suspect rows
//! cut at their bounds with the rest counted, every text cut to [`MAX_TEXT_BYTES`]. The record
//! keeps ids, hashes, counts and computed texts; it copies no block body. Only the card's facts
//! and the tension summaries hold text, and both are computed texts.

use crate::coverage::{Confidence, CoverageIssue, Dimension, Level, Rollup};
use crate::error::E_VALIDATION;
use crate::pending::{Item, card_hash};
use crate::plan::lifecycle::PlanSummary;
use crate::tensions::{TensionInfo, TensionSeverity, TensionStatus};
use crate::{CoverageGate, ProposalRef};
use rdy_model::assessment::{
    Assessment, AssessmentCard, AssessmentConfidence, AssessmentCounts, AssessmentCoverage,
    AssessmentException, AssessmentExceptionKind, AssessmentFacts, AssessmentIssue,
    AssessmentLevel, AssessmentPack, AssessmentPlanAnswerRef, AssessmentPlanCounts,
    AssessmentReversal, AssessmentReversibility, AssessmentScope, AssessmentScores,
    AssessmentSeverity, AssessmentSuspect, AssessmentTension, AssessmentTensionStatus, MAX_ROWS,
    MAX_TEXT_BYTES,
};
use rdy_model::{BlockId, Hash};
use rdy_schema::{Issue, Pack, Severity};
use rdy_store::{codec, kinds};
use std::collections::{BTreeMap, BTreeSet};

/// The computation version of the record.
pub const METHOD: u32 = 1;
/// The most tension rows a record keeps; the rest are counted in `tensions_omitted` (O9).
pub const MAX_TENSION_ROWS: usize = 1_000;
/// The most suspect rows a record keeps; the rest are counted in `suspects_omitted` (O9).
pub const MAX_SUSPECT_ROWS: usize = 1_000;

/// The domain of [`config_digest`].
const CONFIG_DOMAIN: &[u8] = b"rdy-assessment-config-1\0";
/// The domain of [`pack_digest`].
const PACK_DOMAIN: &[u8] = b"rdy-assessment-pack-1\0";
/// What a cut text ends with.
const CUT_MARK: &str = "...";
/// The fix of a reach over [`MAX_ROWS`] and of a record too large to store (O9).
const REACH_FIX: &str = "split the work into smaller proposals";

/// The gate settings a readiness read takes, and the config digest follows (O18):
/// `reviews.require_up_to_date` and `coverage.gate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateConfig {
    pub require_up_to_date: bool,
    pub coverage_gate: CoverageGate,
}

/// What a ready version's readiness read: every input of the record, already computed.
///
/// `pref` is the version (its hash is `pref.version`); the reach bound's issue names it. Each
/// tension comes with its vkey and its status (`ReadState::tension_status`), which
/// [`TensionInfo`] does not carry whole: its dismissal is left out when the review cannot be
/// read, and the row's dismissal is the review all the same (O17). `answers` maps a tension's
/// full id to the hash of the bound plan's answered item (`plan_hash`). The lists may come in
/// any order and with duplicates.
pub struct Inputs<'a> {
    pub pref: ProposalRef,
    pub base: Option<Hash>,
    pub pack: &'a Pack,
    pub gate: GateConfig,
    pub changes: usize,
    pub consequences: usize,
    pub plan: &'a PlanSummary,
    pub plan_hash: Option<Hash>,
    pub supersedes: Vec<BlockId>,
    pub reaffirms: Vec<BlockId>,
    /// Each decision and whether it is one_way.
    pub reversibility: Vec<(BlockId, bool)>,
    pub exceptions: Vec<(AssessmentExceptionKind, String)>,
    /// The tensions of `ReadState::proposal_tensions`, in its order, each with its vkey and
    /// its status.
    pub tensions: &'a [(TensionInfo, Hash, TensionStatus)],
    /// The tensions `proposal_tensions` left out.
    pub tensions_omitted: usize,
    /// (tension id, item hash) for each tension item the bound plan answers.
    pub answers: &'a [(Hash, Hash)],
    /// The rows of the version's earlier assessment that name a bound answer
    /// (`plan::compute::FixedTensions::rows`): one that `tensions` no longer lists is kept, with
    /// its answer from `answers`, so a fresh record never drops a bound item (ruling final F1).
    pub carried: &'a [AssessmentTension],
    /// (suspect block, cause).
    pub suspects: Vec<(BlockId, BlockId)>,
    pub written: BTreeSet<BlockId>,
    pub plan_blocks: BTreeSet<BlockId>,
    pub scope_blocks: BTreeSet<BlockId>,
    pub coverage: AssessmentCoverage,
    pub confidence: AssessmentConfidence,
    pub issues: Vec<AssessmentIssue>,
    /// The approve item the card presented.
    pub card: &'a Item,
}

/// The digest of the configuration the readiness read (O7, O18): the domain, then the JSON
/// `{"coverage_gate": "off" | "no_lower", "require_up_to_date": <bool>}` with its keys in this
/// order.
pub fn config_digest(gate: &GateConfig) -> Hash {
    let json = serde_json::json!({
        "coverage_gate": gate.coverage_gate.as_str(),
        "require_up_to_date": gate.require_up_to_date,
    });
    let mut bytes = CONFIG_DOMAIN.to_vec();
    bytes.extend_from_slice(json.to_string().as_bytes());
    Hash::of(&bytes)
}

/// The digest of the resolved pack (O8). `Pack`'s maps are ordered, so its JSON is stable.
pub fn pack_digest(pack: &Pack) -> Hash {
    let mut bytes = PACK_DOMAIN.to_vec();
    bytes.extend_from_slice(
        &serde_json::to_vec(pack).expect("a pack serializes to JSON: its maps have string keys"),
    );
    Hash::of(&bytes)
}

/// The reach of version `pref` (O18), sorted: the blocks it writes (`written`), what its plan
/// items name (`plan_blocks`), and the blocks of every tension row it lists, open and dismissed
/// (`tension_blocks`, O17). Err: the O9 issue of a reach over [`MAX_ROWS`]. Readiness (check 6)
/// and [`build`] both compute it here, over the same rows, so one check holds for both.
pub(crate) fn reach(
    pref: &ProposalRef,
    written: &BTreeSet<BlockId>,
    plan_blocks: &BTreeSet<BlockId>,
    tension_blocks: impl IntoIterator<Item = BlockId>,
) -> Result<BTreeSet<BlockId>, Issue> {
    let mut reach: BTreeSet<BlockId> = written.iter().chain(plan_blocks).cloned().collect();
    reach.extend(tension_blocks);
    if reach.len() > MAX_ROWS {
        return Err(reach_too_large(pref));
    }
    Ok(reach)
}

/// The issue of a version whose reach exceeds [`MAX_ROWS`] (O9).
pub(crate) fn reach_too_large(pref: &ProposalRef) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_VALIDATION,
        block: None,
        message: format!("{pref} reaches more than {MAX_ROWS} blocks"),
        fix: REACH_FIX.to_string(),
    }
}

/// The issue of a version whose record the store's bounded decoder would refuse (O9).
pub(crate) fn too_large(pref: &ProposalRef) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_VALIDATION,
        block: None,
        message: format!("{pref} is too large to assess"),
        fix: REACH_FIX.to_string(),
    }
}

/// Whether the store can write `a` and read it back: its frame within one record
/// (`codec::fits_latest`), and its body within the bounded decoder's limits, as the store's
/// own assessment check reads it (`codec::split`, `codec::decode_bounded_split`).
fn fits_the_store(a: &Assessment) -> bool {
    if codec::fits_latest(kinds::ASSESSMENT, a).is_err() {
        return false;
    }
    let (raw, payload) = codec::item_new(kinds::ASSESSMENT, a);
    codec::split(raw, &payload)
        .and_then(|o| codec::decode_bounded_split::<Assessment>(&o))
        .is_ok()
}

/// `text` within [`MAX_TEXT_BYTES`]: a longer text keeps its first 4,093 bytes, cut back to a
/// char boundary, and ends with `...` (O9).
pub(crate) fn cut_text(text: String) -> String {
    if text.len() <= MAX_TEXT_BYTES {
        return text;
    }
    let at = text.floor_char_boundary(MAX_TEXT_BYTES - CUT_MARK.len());
    let mut out = text[..at].to_string();
    out.push_str(CUT_MARK);
    out
}

/// The seven scores of a rollup; no rollup (a doc absent on that side) gives seven 100s, and a
/// dimension the rollup lacks reads 100 (O19).
pub(crate) fn scores_of(r: Option<&Rollup>) -> AssessmentScores {
    let Some(r) = r else {
        return AssessmentScores {
            overall: 100,
            grounded: 100,
            consistent: 100,
            current: 100,
            connected: 100,
            complete: 100,
            governed: 100,
        };
    };
    let dim = |d: Dimension| r.dimensions.get(&d).copied().unwrap_or(100);
    AssessmentScores {
        overall: r.score,
        grounded: dim(Dimension::Grounded),
        consistent: dim(Dimension::Consistent),
        current: dim(Dimension::Current),
        connected: dim(Dimension::Connected),
        complete: dim(Dimension::Complete),
        governed: dim(Dimension::Governed),
    }
}

/// A coverage issue as a record row. Its dismissal keeps the review only (O19).
pub(crate) fn issue_of(i: &CoverageIssue) -> AssessmentIssue {
    let code = serde_json::to_value(i.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("an issue code serializes as its snake_case name");
    AssessmentIssue {
        block: BlockId::parse(&i.block).expect("a coverage issue names its block by a block id"),
        dimension: i.dimension.name().to_string(),
        code,
        subject: i.subject.clone(),
        fix: cut_text(i.fix.clone()),
        dismissal: i.dismissed.as_ref().and_then(|d| d.review.parse().ok()),
    }
}

/// A confidence as the record keeps it: its level, and its reasons as rows, sorted and
/// distinct (O19).
pub(crate) fn confidence_record(c: &Confidence) -> AssessmentConfidence {
    AssessmentConfidence {
        level: match c.confidence {
            Level::High => AssessmentLevel::High,
            Level::Medium => AssessmentLevel::Medium,
            Level::Low => AssessmentLevel::Low,
        },
        reasons: issue_rows(c.reasons.iter().map(issue_of).collect()),
    }
}

/// The sort key of an issue row, as [`Assessment::validate`] orders them.
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

/// `rows` with each fix cut, sorted by [`issue_key`], distinct, at most [`MAX_ROWS`].
fn issue_rows(rows: Vec<AssessmentIssue>) -> Vec<AssessmentIssue> {
    let mut rows: Vec<AssessmentIssue> = rows
        .into_iter()
        .map(|mut i| {
            i.fix = cut_text(i.fix);
            i
        })
        .collect();
    rows.sort_by(|a, b| issue_key(a).cmp(&issue_key(b)));
    rows.dedup();
    rows.truncate(MAX_ROWS);
    rows
}

/// `ids` sorted and distinct, at most [`MAX_ROWS`].
fn id_rows(ids: Vec<BlockId>) -> Vec<BlockId> {
    let set: BTreeSet<BlockId> = ids.into_iter().collect();
    set.into_iter().take(MAX_ROWS).collect()
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn facts_of(i: &Inputs<'_>) -> AssessmentFacts {
    let plan = i.plan;
    // a decision named twice is one_way when either names it so
    let mut reversibility: BTreeMap<&BlockId, bool> = BTreeMap::new();
    for (d, one_way) in &i.reversibility {
        *reversibility.entry(d).or_default() |= *one_way;
    }
    let exceptions: BTreeSet<(AssessmentExceptionKind, &str)> =
        i.exceptions.iter().map(|(k, r)| (*k, r.as_str())).collect();
    AssessmentFacts {
        changes: count(i.changes),
        consequences: count(i.consequences),
        plan: AssessmentPlanCounts {
            open: count(plan.open),
            closed: count(plan.closed),
            deferred: count(plan.deferred),
            by_kind: plan
                .by_kind
                .iter()
                .map(|(k, c)| {
                    let counts = AssessmentCounts {
                        open: count(c.open),
                        closed: count(c.closed),
                        deferred: count(c.deferred),
                    };
                    (k.to_string(), counts)
                })
                .collect(),
        },
        supersedes: id_rows(i.supersedes.clone()),
        reaffirms: id_rows(i.reaffirms.clone()),
        reversibility: reversibility
            .into_iter()
            .take(MAX_ROWS)
            .map(|(d, one_way)| AssessmentReversal {
                decision: d.clone(),
                value: if one_way {
                    AssessmentReversibility::OneWay
                } else {
                    AssessmentReversibility::TwoWay
                },
            })
            .collect(),
        exceptions: exceptions
            .into_iter()
            .take(MAX_ROWS)
            .map(|(kind, reference)| AssessmentException {
                kind,
                reference: reference.to_string(),
            })
            .collect(),
    }
}

/// One tension row (O17), or `None` when its id does not parse (never for a row the engine
/// listed). Its status and dismissal come from `status`, the review a dismissed tension names
/// even when [`TensionInfo`] could not read it.
fn tension_row(
    (t, vkey, status): &(TensionInfo, Hash, TensionStatus),
    answers: &BTreeMap<Hash, Hash>,
    plan: Option<Hash>,
) -> Option<AssessmentTension> {
    let id: Hash = t.tension.parse().ok()?;
    let dismissal = match status {
        TensionStatus::Dismissed(review) => Some(*review),
        TensionStatus::Open => None,
    };
    let blocks: BTreeSet<BlockId> = t
        .blocks
        .iter()
        .filter_map(|b| BlockId::parse(b).ok())
        .collect();
    Some(AssessmentTension {
        id,
        kind: t.kind.name().to_string(),
        severity: match t.severity {
            TensionSeverity::Blocking => AssessmentSeverity::Blocking,
            TensionSeverity::Advisory => AssessmentSeverity::Advisory,
        },
        status: if dismissal.is_some() {
            AssessmentTensionStatus::Dismissed
        } else {
            AssessmentTensionStatus::Open
        },
        answer: plan
            .zip(answers.get(&id))
            .map(|(plan, item)| AssessmentPlanAnswerRef { plan, item: *item }),
        dismissal,
        summary: cut_text(t.summary.clone()),
        blocks: blocks.into_iter().take(MAX_ROWS).collect(),
        vkey: *vkey,
    })
}

/// The record of a ready version (D-SDSV2). Err only for O9's bounds: a reach over
/// [`MAX_ROWS`], or a record the store's bounded decoder would refuse. The result passes
/// [`Assessment::validate`].
pub fn build(i: Inputs<'_>) -> Result<Assessment, Issue> {
    // reach (O18): what the version writes, what its plan items name, and the blocks of every
    // tension it lists
    let reach = reach(
        &i.pref,
        &i.written,
        &i.plan_blocks,
        i.tensions
            .iter()
            .flat_map(|(t, _, _)| t.blocks.iter().filter_map(|b| BlockId::parse(b).ok())),
    )?;
    // the caller's order ranks the rows (refusing first): the first MAX_TENSION_ROWS distinct
    // ones are kept, the rest counted
    let answers: BTreeMap<Hash, Hash> = i.answers.iter().copied().collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut tensions: BTreeMap<Hash, AssessmentTension> = BTreeMap::new();
    let mut cut = 0usize;
    for t in i.tensions {
        if !seen.insert(t.0.tension.as_str()) {
            continue;
        }
        match tension_row(t, &answers, i.plan_hash) {
            Some(row) if tensions.len() < MAX_TENSION_ROWS => {
                tensions.insert(row.id, row);
            }
            _ => cut += 1,
        }
    }
    // ruling final F1: a bound row the live set no longer lists keeps its place, while the bound
    // plan still answers its item
    for row in i.carried {
        if tensions.contains_key(&row.id) {
            continue;
        }
        let Some(answer) = i
            .plan_hash
            .zip(answers.get(&row.id))
            .map(|(plan, item)| AssessmentPlanAnswerRef { plan, item: *item })
        else {
            continue;
        };
        if tensions.len() < MAX_TENSION_ROWS {
            let mut row = row.clone();
            row.answer = Some(answer);
            tensions.insert(row.id, row);
        } else {
            cut += 1;
        }
    }
    let tensions_omitted = count(i.tensions_omitted).saturating_add(count(cut));
    let suspects: BTreeSet<(BlockId, BlockId)> = i.suspects.iter().cloned().collect();
    let suspects_omitted = count(suspects.len().saturating_sub(MAX_SUSPECT_ROWS));
    let mut pages: BTreeMap<BlockId, _> = BTreeMap::new();
    for p in i.coverage.pages.iter().cloned() {
        pages.entry(p.doc.clone()).or_insert(p);
    }
    let card = AssessmentCard {
        hash: card_hash(i.card),
        facts: i
            .card
            .facts
            .iter()
            .take(MAX_ROWS)
            .map(|f| cut_text(format!("{}: {}", f.name, f.value)))
            .collect(),
    };
    let a = Assessment {
        proposal: i.pref.version,
        base: i.base,
        method: METHOD,
        pack: AssessmentPack {
            name: i.pack.name.clone(),
            version: i.pack.version.clone(),
            digest: pack_digest(i.pack),
        },
        config: config_digest(&i.gate),
        policy: None,
        facts: facts_of(&i),
        tensions: tensions.into_values().collect(),
        tensions_omitted,
        suspects: suspects
            .into_iter()
            .take(MAX_SUSPECT_ROWS)
            .map(|(block, cause)| AssessmentSuspect { block, cause })
            .collect(),
        suspects_omitted,
        coverage: AssessmentCoverage {
            pages: pages.into_values().take(MAX_ROWS).collect(),
            corpus: i.coverage.corpus.clone(),
        },
        confidence: AssessmentConfidence {
            level: i.confidence.level,
            reasons: issue_rows(i.confidence.reasons.clone()),
        },
        reach: reach.into_iter().collect(),
        card,
        scope: AssessmentScope {
            proposal: i.pref.version,
            blocks: i.scope_blocks.iter().take(MAX_ROWS).cloned().collect(),
        },
        issues: issue_rows(i.issues),
    };
    // the per-list caps do not bound the whole record: check it as the store will
    if !fits_the_store(&a) {
        return Err(too_large(&i.pref));
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::{CoverageIssue, Dimension, Dismissal, IssueCode, Level, Rollup};
    use crate::pending::{Fact, Item, ItemKind, card_hash};
    use crate::tensions::{
        DismissalInfo, TensionInfo, TensionKind, TensionSeverity, TensionStatus,
    };
    use rdy_model::assessment::{
        AssessmentCoverage, AssessmentExceptionKind, AssessmentLevel, AssessmentPage,
        AssessmentReversibility, AssessmentScoreChange, AssessmentScores, MAX_ROWS,
    };
    use rdy_model::{BlockId, Hash};
    use rdy_schema::{Pack, PackSet, Severity};
    use std::collections::{BTreeMap, BTreeSet};

    const BODY: &str = "Guest readers need no account to open a shared page";

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    /// The `n`th valid block id with prefix `p`, in order.
    fn nth(p: &str, mut n: usize) -> BlockId {
        let mut body = [b'0'; 5];
        for c in body.iter_mut().rev() {
            *c = rdy_model::CROCKFORD[n % 32];
            n /= 32;
        }
        id(&format!("{p}-{}", std::str::from_utf8(&body).unwrap()))
    }

    fn h(n: u32) -> Hash {
        Hash::of(&n.to_le_bytes())
    }

    fn pack() -> Pack {
        PackSet::with_builtin().resolve("rdy-standard").unwrap()
    }

    /// The gate settings with `coverage.gate` off.
    fn off(require_up_to_date: bool) -> GateConfig {
        GateConfig {
            require_up_to_date,
            coverage_gate: CoverageGate::Off,
        }
    }

    fn pref() -> ProposalRef {
        ProposalRef {
            version: h(9_001),
            stable: h(9_000),
            n: 2,
        }
    }

    fn fact(name: &'static str, value: &str) -> Fact {
        Fact {
            name: name.into(),
            value: value.to_string(),
        }
    }

    fn card() -> Item {
        Item {
            r#ref: "P-7f3a@2".to_string(),
            kind: ItemKind::Approve,
            title: "Guest mode".to_string(),
            why: "ready to land: 3 changes".to_string(),
            blocking: false,
            summary: Some("Lets guests read shared pages".to_string()),
            summary_by: None,
            facts: vec![
                fact("Changes", "3 changes in 2 docs"),
                fact("Tensions", "0 blocking, 1 advisory"),
                fact("Base", "canon at 0123456789ab, current"),
            ],
            members: vec![],
            options: vec![],
            raised_by: "agent:planner".to_string(),
            raised_at: "2026-10-12T03:04:20Z".to_string(),
            age: "today".to_string(),
            batch: None,
            exception: None,
            card: String::new(),
            card_hash: String::new(),
            removal: None,
            removal_reports: BTreeMap::new(),
            plan_binding: None,
        }
    }

    /// Tension `n` on `blocks`, with its vkey; dismissed by review `h(n + 50_000)` when asked.
    fn tension(n: u32, blocks: &[&str], dismissed: bool) -> (TensionInfo, Hash, TensionStatus) {
        let t = h(n);
        let info = TensionInfo {
            id: format!("tn-{}", t.short()),
            tension: t.to_string(),
            kind: TensionKind::UnjustifiedChange,
            severity: TensionSeverity::Advisory,
            overridden: false,
            status: if dismissed { "dismissed" } else { "open" }.to_string(),
            summary: format!("tension {n}"),
            sides: vec![],
            blocks: blocks.iter().map(ToString::to_string).collect(),
            dismissal: dismissed.then(|| DismissalInfo {
                review: h(n + 50_000).to_string(),
                actor: "person:nick".to_string(),
                run: None,
                role: "person".to_string(),
                at: "2026-10-12T03:04:20Z".to_string(),
                at_ms: 0,
                reason: "checked".to_string(),
            }),
            refuses: Some(false),
            read_time: false,
            proposal: None,
            judged: None,
        };
        let status = if dismissed {
            TensionStatus::Dismissed(h(n + 50_000))
        } else {
            TensionStatus::Open
        };
        (info, h(n + 100_000), status)
    }

    fn scores(n: u32) -> AssessmentScores {
        AssessmentScores {
            overall: n,
            grounded: n,
            consistent: n,
            current: n,
            connected: n,
            complete: n,
            governed: n,
        }
    }

    fn issue(block: &str, code: &str, fix: &str) -> AssessmentIssue {
        AssessmentIssue {
            block: id(block),
            dimension: "grounded".to_string(),
            code: code.to_string(),
            subject: None,
            fix: fix.to_string(),
            dismissal: None,
        }
    }

    /// What every test builds from: owned parts the inputs borrow.
    struct Fx {
        pack: Pack,
        plan: PlanSummary,
        card: Item,
        tensions: Vec<(TensionInfo, Hash, TensionStatus)>,
        answers: Vec<(Hash, Hash)>,
    }

    fn fx() -> Fx {
        Fx {
            pack: pack(),
            plan: PlanSummary::default(),
            card: card(),
            // out of id order on purpose
            tensions: vec![
                tension(3, &["n-00003", "s-0000T"], false),
                tension(1, &["n-00001"], true),
                tension(2, &["d-00002"], false),
            ],
            answers: vec![(h(3), h(70_003))],
        }
    }

    fn inputs(f: &Fx) -> Inputs<'_> {
        Inputs {
            pref: pref(),
            base: Some(h(8_000)),
            pack: &f.pack,
            gate: GateConfig {
                require_up_to_date: false,
                coverage_gate: CoverageGate::Off,
            },
            changes: 3,
            consequences: 2,
            plan: &f.plan,
            plan_hash: Some(h(7_000)),
            supersedes: vec![id("d-0000B"), id("d-0000A"), id("d-0000B")],
            reaffirms: vec![id("d-0000D"), id("d-0000C")],
            reversibility: vec![(id("d-0000D"), true), (id("d-0000C"), false)],
            exceptions: vec![
                (
                    AssessmentExceptionKind::OneWay,
                    "d-0000D@0123456789ab".to_string(),
                ),
                (AssessmentExceptionKind::Against, "d-0000C".to_string()),
            ],
            tensions: &f.tensions,
            tensions_omitted: 0,
            answers: &f.answers,
            carried: &[],
            suspects: vec![
                (id("n-00009"), id("d-0000A")),
                (id("n-00008"), id("d-0000A")),
            ],
            written: [id("n-00005"), id("n-00004")].into_iter().collect(),
            plan_blocks: [id("n-00007"), id("d-0000A")].into_iter().collect(),
            scope_blocks: [id("n-00005"), id("n-00004"), id("n-00006")]
                .into_iter()
                .collect(),
            coverage: AssessmentCoverage {
                pages: vec![
                    AssessmentPage {
                        doc: id("dc-0000Y"),
                        before: scores(100),
                        after: scores(90),
                    },
                    AssessmentPage {
                        doc: id("dc-0000X"),
                        before: scores(80),
                        after: scores(85),
                    },
                ],
                corpus: AssessmentScoreChange {
                    before: scores(95),
                    after: scores(93),
                },
            },
            confidence: AssessmentConfidence {
                level: AssessmentLevel::Medium,
                reasons: vec![
                    issue("n-00005", "untied_content", "tie it"),
                    issue("n-00004", "untied_content", "tie it"),
                ],
            },
            issues: vec![
                issue("n-00005", "untied_content", "tie it"),
                issue("n-00004", "untied_content", "tie it"),
                issue("n-00005", "untied_content", "tie it"),
            ],
            card: &f.card,
        }
    }

    fn ids(v: &[&str]) -> Vec<BlockId> {
        v.iter().map(|s| id(s)).collect()
    }

    #[test]
    fn rg6_a_built_assessment_passes_validate() {
        let f = fx();
        let a = build(inputs(&f)).unwrap();
        assert_eq!(a.validate(), Ok(()));
        assert_eq!(a.proposal, pref().version);
        assert_eq!(a.scope.proposal, pref().version);
        assert_eq!(a.base, Some(h(8_000)));
        assert_eq!(a.method, METHOD);
        assert_eq!(a.pack.name, f.pack.name);
        assert_eq!(a.pack.version, f.pack.version);
        assert_eq!(a.pack.digest, pack_digest(&f.pack));
        assert_eq!(a.config, config_digest(&off(false)));
        assert_eq!(a.policy, None);
        assert_eq!((a.facts.changes, a.facts.consequences), (3, 2));
        assert_eq!(a.facts.supersedes, ids(&["d-0000A", "d-0000B"]));
        assert_eq!(a.facts.reaffirms, ids(&["d-0000C", "d-0000D"]));
        let rev: Vec<_> = a
            .facts
            .reversibility
            .iter()
            .map(|r| (r.decision.to_string(), r.value))
            .collect();
        assert_eq!(
            rev,
            vec![
                ("d-0000C".to_string(), AssessmentReversibility::TwoWay),
                ("d-0000D".to_string(), AssessmentReversibility::OneWay),
            ]
        );
        assert_eq!(a.facts.exceptions[0].kind, AssessmentExceptionKind::Against);
        let kinds: Vec<&str> = a
            .facts
            .plan
            .by_kind
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        let mut sorted = kinds.clone();
        sorted.sort();
        assert_eq!(kinds, sorted);
        // the tension rows: by id, each with its status, dismissal and answer
        let mut want: Vec<Hash> = vec![h(1), h(2), h(3)];
        want.sort();
        assert_eq!(a.tensions.iter().map(|t| t.id).collect::<Vec<_>>(), want);
        let row = |n: u32| a.tensions.iter().find(|t| t.id == h(n)).unwrap();
        assert_eq!(row(1).status, AssessmentTensionStatus::Dismissed);
        assert_eq!(row(1).dismissal, Some(h(50_001)));
        assert_eq!(row(1).answer, None);
        assert_eq!(row(2).status, AssessmentTensionStatus::Open);
        assert_eq!(
            row(3).answer,
            Some(AssessmentPlanAnswerRef {
                plan: h(7_000),
                item: h(70_003),
            })
        );
        assert_eq!(row(3).vkey, h(100_003));
        assert_eq!(row(3).kind, "unjustified_change");
        assert_eq!(row(3).severity, AssessmentSeverity::Advisory);
        assert_eq!(row(3).blocks, ids(&["n-00003", "s-0000T"]));
        assert_eq!(a.tensions_omitted, 0);
        let sus: Vec<String> = a.suspects.iter().map(|s| s.block.to_string()).collect();
        assert_eq!(sus, vec!["n-00008", "n-00009"]);
        assert_eq!(a.suspects_omitted, 0);
        let pages: Vec<String> = a.coverage.pages.iter().map(|p| p.doc.to_string()).collect();
        assert_eq!(pages, vec!["dc-0000X", "dc-0000Y"]);
        assert_eq!(a.confidence.level, AssessmentLevel::Medium);
        assert_eq!(a.confidence.reasons.len(), 2);
        assert_eq!(a.scope.blocks, ids(&["n-00004", "n-00005", "n-00006"]));
        // the duplicate issue goes; the rest are by block
        assert_eq!(
            a.issues
                .iter()
                .map(|i| i.block.to_string())
                .collect::<Vec<_>>(),
            vec!["n-00004", "n-00005"]
        );
    }

    #[test]
    fn rg6_a_dismissed_row_whose_review_cannot_be_read_keeps_its_review() {
        // the review's reason was redacted: `tension_info_bare` leaves the dismissal out, while
        // the status still names the review (TensionStatus::Dismissed)
        let mut f = fx();
        let review = h(77);
        let t = &mut f.tensions[1];
        t.0.dismissal = None;
        t.2 = TensionStatus::Dismissed(review);
        let a = build(inputs(&f)).unwrap();
        assert_eq!(a.validate(), Ok(()));
        let row = a.tensions.iter().find(|t| t.id == h(1)).unwrap();
        assert_eq!(row.status, AssessmentTensionStatus::Dismissed);
        assert_eq!(row.dismissal, Some(review));
        // the status decides, not the info's text: an open status is an open row
        let mut g = fx();
        g.tensions[1].2 = TensionStatus::Open;
        let a = build(inputs(&g)).unwrap();
        let row = a.tensions.iter().find(|t| t.id == h(1)).unwrap();
        assert_eq!(row.status, AssessmentTensionStatus::Open);
        assert_eq!(row.dismissal, None);
    }

    #[test]
    fn rg6_the_same_inputs_give_the_same_bytes() {
        let f = fx();
        let a = build(inputs(&f)).unwrap();
        let b = build(inputs(&f)).unwrap();
        assert_eq!(
            postcard::to_allocvec(&a).unwrap(),
            postcard::to_allocvec(&b).unwrap()
        );
        // the order the caller gives its lists in does not matter
        let mut g = fx();
        g.tensions.reverse();
        let mut i = inputs(&g);
        i.supersedes.reverse();
        i.reaffirms.reverse();
        i.reversibility.reverse();
        i.exceptions.reverse();
        i.suspects.reverse();
        i.coverage.pages.reverse();
        i.confidence.reasons.reverse();
        i.issues.reverse();
        let c = build(i).unwrap();
        assert_eq!(
            postcard::to_allocvec(&a).unwrap(),
            postcard::to_allocvec(&c).unwrap()
        );
    }

    #[test]
    fn rg6_tensions_past_1000_are_counted_in_tensions_omitted() {
        let mut f = fx();
        f.tensions = (0..1_003)
            .map(|n| tension(n, &["n-00001"], false))
            .collect();
        let suspects: Vec<(BlockId, BlockId)> =
            (0..1_004).map(|n| (nth("n", n), id("d-0000A"))).collect();
        let mut i = inputs(&f);
        i.tensions_omitted = 2;
        i.suspects = suspects;
        let a = build(i).unwrap();
        assert_eq!(a.validate(), Ok(()));
        assert_eq!(a.tensions.len(), MAX_TENSION_ROWS);
        assert_eq!(a.tensions_omitted, 5);
        // the rows kept are the first 1,000 the caller gave (its order ranks them)
        let kept: BTreeSet<Hash> = a.tensions.iter().map(|t| t.id).collect();
        let first: BTreeSet<Hash> = (0..1_000).map(h).collect();
        assert_eq!(kept, first);
        assert_eq!(a.suspects.len(), MAX_SUSPECT_ROWS);
        assert_eq!(a.suspects_omitted, 4);
        assert_eq!(a.suspects[0].block, nth("n", 0));
    }

    #[test]
    fn rg6_reach_is_written_plus_plan_plus_tension_blocks_sorted() {
        let f = fx();
        let a = build(inputs(&f)).unwrap();
        // written n-00004, n-00005; plan n-00007, d-0000A; tensions n-00001, d-00002,
        // n-00003, s-0000T; scope-only n-00006 is not in it
        assert_eq!(
            a.reach,
            ids(&[
                "d-00002", "d-0000A", "n-00001", "n-00003", "n-00004", "n-00005", "n-00007",
                "s-0000T",
            ])
        );
    }

    #[test]
    fn rg6_a_reach_over_max_rows_is_not_ready() {
        let f = fx();
        let mut i = inputs(&f);
        i.written = (0..MAX_ROWS - 6).map(|n| nth("w", n)).collect();
        // 6 blocks come from the plan and the tensions: exactly MAX_ROWS
        let a = build(i).unwrap();
        assert_eq!(a.reach.len(), MAX_ROWS);
        let mut i = inputs(&f);
        i.written = (0..MAX_ROWS - 5).map(|n| nth("w", n)).collect();
        let e = build(i).unwrap_err();
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.code, crate::E_VALIDATION);
        assert_eq!(e.block, None);
        assert_eq!(
            e.message,
            format!("P-{}@2 reaches more than 100000 blocks", h(9_000).short())
        );
        assert_eq!(e.fix, "split the work into smaller proposals");
    }

    /// `n` unlinked_mention rows, one on each of `n` blocks, with a subject and a fix of the
    /// engine's length (about 85 bytes).
    fn mentions(n: usize) -> Vec<AssessmentIssue> {
        (0..n)
            .map(|k| AssessmentIssue {
                block: nth("n", k),
                dimension: "grounded".to_string(),
                code: "unlinked_mention".to_string(),
                subject: Some(nth("e", k).to_string()),
                fix: format!(
                    "link this mention to Payments service ({}), or dismiss it as a passing mention",
                    nth("e", k)
                ),
                dismissal: None,
            })
            .collect()
    }

    #[test]
    fn rg6_a_record_past_the_decoder_bound_is_not_ready() {
        // within every per-list cap (O9), past the store's bounded decoder: reach and
        // scope.blocks at 100,000 and 100,000 unlinked_mention rows
        let f = fx();
        let mut i = inputs(&f);
        i.written = (0..MAX_ROWS - 6).map(|n| nth("w", n)).collect();
        i.scope_blocks = (0..MAX_ROWS).map(|n| nth("n", n)).collect();
        i.issues = mentions(MAX_ROWS);
        let Err(e) = build(i) else {
            panic!("a record past the decoder bound was built");
        };
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.code, crate::E_VALIDATION);
        assert_eq!(e.block, None);
        assert_eq!(
            e.message,
            format!("P-{}@2 is too large to assess", h(9_000).short())
        );
        assert_eq!(e.fix, "split the work into smaller proposals");
        // half the blocks with the same issues fits: the check refuses only past the bound
        let mut i = inputs(&f);
        i.written = (0..MAX_ROWS / 2).map(|n| nth("w", n)).collect();
        i.scope_blocks = (0..MAX_ROWS / 2).map(|n| nth("n", n)).collect();
        i.issues = mentions(MAX_ROWS);
        let a = build(i).unwrap();
        assert_eq!(a.validate(), Ok(()));
        assert_eq!(a.issues.len(), MAX_ROWS);
    }

    #[test]
    fn rg6_the_card_facts_are_name_colon_value_in_order() {
        let f = fx();
        let a = build(inputs(&f)).unwrap();
        assert_eq!(
            a.card.facts,
            vec![
                "Changes: 3 changes in 2 docs",
                "Tensions: 0 blocking, 1 advisory",
                "Base: canon at 0123456789ab, current",
            ]
        );
        assert_eq!(a.card.hash, card_hash(&f.card));
    }

    #[test]
    fn rg6_a_long_fact_is_cut_to_4096_bytes() {
        let mut f = fx();
        // "ab: " is 4 bytes, so each é (2 bytes) starts at an even offset and byte 4,093 is
        // inside one: the cut falls back to 4,092
        f.card.facts = vec![
            fact("ab", &"é".repeat(3_000)),
            fact("c", &"x".repeat(4_093)),
            fact("d", &"x".repeat(5_000)),
        ];
        let a = build(inputs(&f)).unwrap();
        assert_eq!(a.validate(), Ok(()));
        let long = &a.card.facts[0];
        assert_eq!(long.len(), 4_095);
        assert!(long.ends_with("..."));
        assert_eq!(
            &long[..4_092],
            &format!("ab: {}", "é".repeat(3_000))[..4_092]
        );
        // a fact of exactly 4,096 bytes stays whole
        assert_eq!(a.card.facts[1], format!("c: {}", "x".repeat(4_093)));
        assert_eq!(a.card.facts[2].len(), 4_096);
        assert_eq!(a.card.facts[2], format!("d: {}...", "x".repeat(4_090)));
    }

    #[test]
    fn rg6_config_digest_changes_with_require_up_to_date() {
        assert_ne!(config_digest(&off(true)), config_digest(&off(false)));
        assert_eq!(
            config_digest(&off(true)),
            Hash::of(
                b"rdy-assessment-config-1\0{\"coverage_gate\":\"off\",\"require_up_to_date\":true}"
            )
        );
        assert_eq!(
            config_digest(&off(false)),
            Hash::of(
                b"rdy-assessment-config-1\0{\"coverage_gate\":\"off\",\"require_up_to_date\":false}"
            )
        );
    }

    #[test]
    fn ap3_config_digest_moves_with_the_coverage_gate() {
        let no_lower = GateConfig {
            require_up_to_date: false,
            coverage_gate: CoverageGate::NoLower,
        };
        assert_ne!(config_digest(&no_lower), config_digest(&off(false)));
        assert_eq!(
            config_digest(&no_lower),
            Hash::of(
                b"rdy-assessment-config-1\0{\"coverage_gate\":\"no_lower\",\"require_up_to_date\":false}"
            )
        );
    }

    #[test]
    fn rg6_pack_digest_changes_with_a_tension_severity() {
        let p = pack();
        let mut q = pack();
        q.tensions
            .insert(TensionKind::Fork, TensionSeverity::Advisory);
        assert_ne!(pack_digest(&p), pack_digest(&q));
        let mut want = b"rdy-assessment-pack-1\0".to_vec();
        want.extend_from_slice(&serde_json::to_vec(&p).unwrap());
        assert_eq!(pack_digest(&p), Hash::of(&want));
        assert_eq!(pack_digest(&p), pack_digest(&pack()));
    }

    #[test]
    fn rg6_the_record_copies_no_block_body() {
        let mut f = fx();
        // the body reaches the card as its title, why, summary, members, text and a fact, and a
        // tension's summary quotes it
        f.card.title = BODY.to_string();
        f.card.why = BODY.to_string();
        f.card.summary = Some(BODY.to_string());
        f.card.members = vec![format!("D-0931  {BODY}")];
        f.card.card = BODY.to_string();
        f.card.facts.push(fact("Statement", BODY));
        f.tensions[0].0.summary = format!("restates: {BODY}");
        let mut a = build(inputs(&f)).unwrap();
        assert!(a.card.facts.iter().any(|x| x.contains(BODY)));
        assert!(a.tensions.iter().any(|t| t.summary.contains(BODY)));
        a.card.facts.clear();
        for t in &mut a.tensions {
            t.summary.clear();
        }
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains(BODY), "{json}");
        assert!(!json.contains("Guest readers"), "{json}");
    }

    #[test]
    fn rg6_scores_of_reads_100_where_a_rollup_lacks_a_dimension() {
        assert_eq!(scores_of(None), scores(100));
        let r = Rollup {
            score: 72,
            dimensions: [(Dimension::Grounded, 40), (Dimension::Governed, 0)]
                .into_iter()
                .collect(),
        };
        assert_eq!(
            scores_of(Some(&r)),
            AssessmentScores {
                overall: 72,
                grounded: 40,
                consistent: 100,
                current: 100,
                connected: 100,
                complete: 100,
                governed: 0,
            }
        );
    }

    #[test]
    fn rg6_issue_of_maps_each_field() {
        let review = h(5);
        let i = CoverageIssue {
            block: "n-00004".to_string(),
            dimension: Dimension::Consistent,
            code: IssueCode::DismissedTension,
            subject: Some("tn-0123456789ab".to_string()),
            fix: "y".repeat(5_000),
            dismissed: Some(Dismissal {
                review: review.to_string(),
                actor: "person:nick".to_string(),
                reason: "checked".to_string(),
            }),
        };
        let a = issue_of(&i);
        assert_eq!(a.block, id("n-00004"));
        assert_eq!(a.dimension, "consistent");
        assert_eq!(a.code, "dismissed_tension");
        assert_eq!(a.subject.as_deref(), Some("tn-0123456789ab"));
        assert_eq!(a.fix, format!("{}...", "y".repeat(4_093)));
        assert_eq!(a.dismissal, Some(review));
    }

    #[test]
    fn rg6_confidence_reasons_are_sorted_and_distinct() {
        let reason = |block: &str, code: IssueCode| CoverageIssue {
            block: block.to_string(),
            dimension: code.dimension(),
            code,
            subject: None,
            fix: "fix it".to_string(),
            dismissed: None,
        };
        let c = crate::coverage::Confidence {
            confidence: Level::Low,
            reasons: vec![
                reason("n-00005", IssueCode::OpenSuspect),
                reason("n-00004", IssueCode::UntiedContent),
                reason("n-00005", IssueCode::OpenSuspect),
            ],
            reasons_omitted: 3,
        };
        let r = confidence_record(&c);
        assert_eq!(r.level, AssessmentLevel::Low);
        let got: Vec<(String, String)> = r
            .reasons
            .iter()
            .map(|i| (i.block.to_string(), i.code.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("n-00004".to_string(), "untied_content".to_string()),
                ("n-00005".to_string(), "open_suspect".to_string()),
            ]
        );
    }
}
