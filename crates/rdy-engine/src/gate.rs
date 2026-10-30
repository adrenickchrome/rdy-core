use crate::alt::{AltPick, alt_label};
use crate::answer::card_of;
use crate::answer::relay::{RelayInput, relayed};
use crate::coord_graph::AltRef;
use crate::error::*;
use crate::pending::queue::approval_holds;
use crate::pending::{Item, PlanCardBinding};
use crate::plan::lifecycle::{PlanSummary, ProposePlanRequest};
use crate::readiness::assessment::{config_digest, pack_digest};
use crate::session::rfc3339;
use crate::tensions::{SideRef, Tension, TensionInfo, TensionSeverity, TensionStatus};
use crate::views::{
    Action, BaseBlocks, ConflictInfo, conflict_infos, creators, landings_through, missing_parents,
    parse_change,
};
use crate::{
    Coverage, Delta, Materialized, OpInput, Rdy, ReadState, Scope, Session, WriteRequest,
    WriteResult, rfc3339_now,
};
use rdy_model::{
    ActorRun, AltChoice, AltSet, AltStatus, Answer, AnswerAct, ApprovalEvidence, Block, BlockId,
    Change, ChangeLinks, EvidenceKind, FieldValue, Hash, LandingRecord, Op, Proposal, decision,
};
use rdy_schema::{Issue, Severity, validate_prefix, validate_read_block};
use rdy_store::{AltLanding, Conflict, LandRequest, MergeChange, Relay, merge_on_blocks};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// A proposal version as an approver names it: the version's hash, the proposal's stable id (its
/// first version's hash) and the version number, counted from 1. Shown as `P-<stable short>@<n>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProposalRef {
    pub version: Hash,
    pub stable: Hash,
    pub n: u32,
}

impl std::fmt::Display for ProposalRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "P-{}@{}", self.stable.short(), self.n)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProposalInfo {
    /// The version's hash.
    pub id: String,
    /// The proposal's stable id (its first version's hash).
    pub stable: String,
    /// The version number, counted from 1.
    pub version: u32,
    /// `P-<stable short>@<version>`: what an approver approves.
    pub r#ref: String,
    pub title: String,
    /// The proposer's plain-words summary, kept from the last version when none is given.
    pub summary: Option<String>,
    pub changes: usize,
    pub warnings: Vec<Issue>,
    /// Every conflict of the view.
    pub conflicts: Vec<ConflictInfo>,
    /// The conflicts this version carries on the blocks it writes: they refuse its landing
    /// (an `E_CONFLICT_UNRESOLVED` issue of `E_PROPOSAL_NOT_READY`) until resolved in the view
    /// and proposed again.
    pub blocking: Vec<ConflictInfo>,
    /// The tensions that involve one of the version's changes, each with `refuses`: the
    /// refusing ones first (they refuse its landing, an `E_BLOCKING_TENSION` issue; read from
    /// what the version lands, its landing form), then by id; at most `PROPOSAL_TENSIONS_MAX`.
    /// A kind judged on the net change (unjustified_change, FW-JU1) is listed only in its
    /// landing form.
    /// A tension whose blocks all carry a blocking conflict is left out: the conflict reports it.
    pub tensions: Vec<TensionInfo>,
    /// The tensions left out past `PROPOSAL_TENSIONS_MAX`.
    #[serde(skip_serializing_if = "is_zero")]
    pub tensions_omitted: usize,
    /// The digest of every advised semantic tension of the version (D-WMCFW), listed or not:
    /// an acceptance passes it back, so it answers for the set the person was shown. `None`
    /// when the version has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advised_digest: Option<String>,
    /// The advised tensions not yet accepted that `tensions` leaves out past the cut.
    #[serde(skip_serializing_if = "is_zero")]
    pub advised_omitted: usize,
    /// Canon's head when the version was written (FW-HB1).
    pub canon_at: Option<String>,
    /// The alternative set the version chooses its own view's member of (altg1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternative: Option<AltRef>,
    /// Keys rdy re-derived when proposing, by block id: a key rdy gave a doc or folder this
    /// version creates that another block holds (K1, FW-PR3).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, String>,
    /// Why the version's tensions could not be read, as `<code>: <message>`: `tensions` is then
    /// empty and the proposal stands (a read error never fails the reply).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tensions_error: Option<String>,
    /// Why the coverage change or the unapplied-decision warnings could not be read, as
    /// `<code>: <message>`; the proposal stands. The coverage change itself (FW-CH3) is read
    /// after the write, off the writer, and joins the reply in `propose_and_report`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_error: Option<String>,
    /// The agent that relayed the person's proposal of their own view (`propose_as`, O32).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<ActorRun>,
    /// The hash of the `ANSWER` that holds that relay, written with the version (O14).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// The change plan the version bound, written in its batch (D-4JMPH): the counts over
    /// every item, the code files and the open items.
    pub plan: PlanSummary,
    /// Whether the version is ready (D-ME2HD, D-3JZP4): it passed every readiness check, so it
    /// has its assessment and its approval card. One that is not ready is stored with no card.
    pub ready: bool,
    /// The readiness issues, in the closed order of [`crate::readiness`]: empty when ready.
    pub issues: Vec<Issue>,
    /// The hash of the `ASSESSMENT` record written in the version's batch (D-SDSV2); none for a
    /// version that is not ready.
    pub assessment: Option<String>,
    /// The authority policy's verdict the assessment stores (D-ZWP7Q, O13); null for a version
    /// that is not ready.
    pub policy: Option<crate::readiness::ProposalPolicy>,
}

/// One view a landing rebased: how many of its changes dropped out (they landed) and how many
/// conflicts its remaining changes have with the new canon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RebasedInfo {
    pub view: String,
    pub dropped: usize,
    pub conflicts: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct LandInfo {
    pub landing: String,
    /// The version approved, as `P-<stable short>@<n>`.
    pub approved: String,
    /// The approved version's hash (the id its consequences carry).
    pub version: String,
    /// The landed version's title, summary and change count.
    pub title: String,
    pub summary: Option<String>,
    pub changes: usize,
    /// `local`, or `relayed` when an agent relayed the human's approval (FW-B9), or `policy` when
    /// an agent landed a version the authority policy accepts (`rdy_land`, D-ZWP7Q).
    pub assurance: String,
    pub warnings: Vec<Issue>,
    pub rebased: Vec<RebasedInfo>,
    /// The alternative set the landing chose its proposing view's member of (altg1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen: Option<AltRef>,
    /// The set's other member views the landing closed, in view order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<String>,
    /// Keys rdy re-derived when landing, by block id: a key rdy gave a doc or folder the version
    /// creates that a block landed since the proposal holds (K1, FW-PR3).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, String>,
}

/// How an approval treats a canon that moved since the version was proposed (FW-HB1), and the
/// relay that carried it (FW-B9).
#[derive(Clone, Debug, Default)]
pub struct LandOptions {
    /// Refuse any landing since the version was proposed, not only one on its blocks.
    pub strict: bool,
    /// The agent that relayed the human's approval, and what the human said.
    pub relay: Option<Relay>,
    /// How the approval was obtained. A relay from the command line is `text`, which is what a
    /// relay with no evidence kind gets.
    pub evidence: Option<ApprovalEvidence>,
}

/// Who lands a version (D-ZWP7Q "The agent's landing act"): a person's approval, or an agent
/// session under the authority policy (`rdy_land`), whose acceptance replaces the approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lander {
    Approver,
    Policy,
}

/// Who relayed an approval, as shown in a landing row.
#[derive(Clone, Debug, Serialize)]
pub struct RelayedByRow {
    pub actor: String,
    pub run: Option<String>,
}

/// What a relayed approval quoted, as shown in a landing row.
#[derive(Clone, Debug, Serialize)]
pub struct RelayEvidenceRow {
    pub quoted: String,
    pub session_ref: String,
    pub question: Option<String>,
}

/// How an approval was obtained, as shown in a landing row.
#[derive(Clone, Debug, Serialize)]
pub struct EvidenceRow {
    pub kind: String,
    /// The options offered; `null` when there were none.
    pub options: Option<Vec<String>>,
    pub choice: Option<String>,
    pub card_hash: Option<String>,
}

/// One landing as `rdy log` shows it (H3-C5). Every key is always present, in this order; a
/// member that does not apply is `null`, except `chosen`, which only a landing that chose an
/// alternative has (altg1). Not a stored type: the record is postcard.
#[derive(Clone, Debug, Serialize)]
pub struct LandingRow {
    pub landing: String,
    pub prev: Option<String>,
    /// The landed proposal version.
    pub proposal: String,
    pub approved_version: Option<String>,
    /// `P-<stable short>@<n>` of `approved_version`.
    pub approved: Option<String>,
    pub title: String,
    /// The proposer's summary of the landed version.
    pub summary: Option<String>,
    /// The approving actor.
    pub approved_by: String,
    /// The provenance of the proposal version that landed.
    pub proposed_by: RelayedByRow,
    pub assurance: String,
    pub relayed_by: Option<RelayedByRow>,
    pub relay_evidence: Option<RelayEvidenceRow>,
    pub evidence: Option<EvidenceRow>,
    /// The stored assessment of the approved version (D-ME2HD); `null` for a landing that holds
    /// none (a re-affirmation card's landing, a host's `land_atomic` landing).
    pub assessment: Option<String>,
    pub rebased_from: Vec<String>,
    /// When the landing was approved, RFC 3339 UTC.
    pub at: String,
    pub at_ms: i64,
    pub changes: usize,
    pub tags: Vec<String>,
    /// The alternative set member the landing chose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen: Option<AltRef>,
}

/// A 32-byte ref value as a hash.
fn ref_hash(bytes: &[u8], what: &str) -> Result<Hash, RdyError> {
    <[u8; 32]>::try_from(bytes)
        .map(Hash)
        .map_err(|_| rdy_store::StoreError::Decode(format!("{what} is not a hash")).into())
}

/// The latest version of the proposal whose stable id is `stable` (its `proposal/` ref).
pub(crate) fn latest_version(st: &ReadState, stable: &Hash) -> Result<Hash, RdyError> {
    match st.reader().get_ref(&format!("proposal/{stable}"))? {
        Some(b) => ref_hash(&b, "a proposal ref"),
        None => Err(rdy_store::StoreError::Decode(format!(
            "proposal {stable} has no ref; run rdy reindex"
        ))
        .into()),
    }
}

/// Version `h`'s number: 1 plus the length of its `prev` chain.
pub(crate) fn version_number(st: &ReadState, h: &Hash) -> Result<u32, RdyError> {
    let mut n = 1;
    let mut cur = st.reader().proposal(h)?.prev;
    while let Some(p) = cur {
        n += 1;
        cur = st.reader().proposal(&p)?.prev;
    }
    Ok(n)
}

/// `pref` is not its proposal's latest version: name the latest, and how to approve it.
pub(crate) fn moved(
    st: &ReadState,
    pref: &ProposalRef,
    latest: &Hash,
    view: &str,
) -> Result<RdyError, RdyError> {
    let now = ProposalRef {
        version: *latest,
        stable: pref.stable,
        n: version_number(st, latest)?,
    };
    Ok(RdyError::new(
        E_PROPOSAL_MOVED,
        format!("proposal {pref} moved: the latest version is {now}"),
        format!("review {now} (rdy_propose's output, or rdy view show {view}), then approve {now}"),
    ))
}

/// The freshness check of removal request `request` for the landing of `pref`
/// (`redaction::check_fresh`, with `prospective` the landing's own new changes). A stale report
/// is `E_REDACTION_STALE`, never refreshed, and its `corrected` holds the corrected card when it
/// can be computed: `{"ref", "request", "report"}`, the version and the stale request, with the
/// complete report a new request with the same targets and reason would record now (the dry
/// run's result). Nothing is written and the approval is not tried again.
fn fresh_removal(
    st: &ReadState,
    request: &Hash,
    prospective: &[Change],
    pref: &ProposalRef,
) -> Result<(), RdyError> {
    crate::redaction::check_fresh(st, request, prospective).map_err(|e| {
        if e.code != E_REDACTION_STALE {
            return e;
        }
        match corrected_removal(st, request, pref) {
            Some(card) => e.with_corrected(card),
            None => e,
        }
    })
}

/// The corrected card of stale removal request `request` of `pref` (see [`fresh_removal`]), or
/// none when the request or the fresh report cannot be read.
fn corrected_removal(
    st: &ReadState,
    request: &Hash,
    pref: &ProposalRef,
) -> Option<serde_json::Value> {
    use crate::redaction::{RedactInput, dry_run, targets};
    let r = st.reader().redaction_request(request).ok()?;
    let input = RedactInput {
        targets: r.targets.iter().map(targets::selector_text).collect(),
        reason_class: targets::reason_class_name(r.reason_class).to_string(),
        reason: r.reason.clone(),
        dry_run: true,
    };
    let report = serde_json::to_value(dry_run(st, &input).ok()?).ok()?;
    Some(serde_json::json!({
        "ref": pref.to_string(),
        "request": request.to_string(),
        "report": report,
    }))
}

/// A store refusal on the approve path, worded for the approver: an `E_STALE_BASE` or
/// `E_ALREADY_EXISTS` keeps its code, names the block and the version's view `view`, and gives
/// the author's fix, never "re-read and retry", which is a writer's. Any other error converts as
/// usual.
fn approver_stale(e: rdy_store::StoreError, view: &str) -> RdyError {
    use rdy_store::StoreError;
    match e {
        StoreError::StaleBase {
            id,
            expected,
            actual: Some(actual),
        } => RdyError::new(
            E_STALE_BASE,
            format!(
                "{id} changed in canon after this version's base: the version writes it from {}, canon has {}",
                expected.short(),
                actual.short()
            ),
            format!(
                "the version no longer applies: its author must update {id} from canon's version in view {view} (or, if {view} is pinned, in a view that reads canon) and propose again; then approve the new version"
            ),
        ),
        StoreError::StaleBase {
            id,
            expected,
            actual: None,
        } => RdyError::new(
            E_STALE_BASE,
            format!(
                "{id} was deleted in canon after this version's base ({})",
                expected.short()
            ),
            format!(
                "its author must drop the op on {id}, or re-create it from a view that reads canon, and propose again; then approve the new version"
            ),
        ),
        StoreError::DeletedSince(id) => RdyError::new(
            E_STALE_BASE,
            format!(
                "{id} was deleted in canon after the base view {view} reads; landing this version would bring it back without having seen that delete"
            ),
            format!(
                "its author must re-create {id} from a view that reads canon, or drop the change that creates it, and propose again; then approve the new version"
            ),
        ),
        StoreError::AlreadyExists(id) => RdyError::new(
            E_ALREADY_EXISTS,
            format!("{id} was created in canon after this version's base"),
            format!(
                "its author must drop the create of {id}, or update canon's {id} in view {view}, and propose again; then approve the new version"
            ),
        ),
        other => other.into(),
    }
}

/// FW-B9: what a relayed approval must satisfy, and the evidence it records. Only a human
/// session records a relay, and it names the exact version it relays (the one the human was
/// shown): a full hash or `P-<id>@<n>`. Returns the evidence to store: the options' own for a
/// local approval, `text` for a relay that names none. A relay's evidence is `text`, or `card`
/// with the hash of the card the person answered (O20: only `answer` passes one); `elicited`
/// and a card with no hash are refused.
fn relay_evidence_kind(
    s: &Session,
    text: &str,
    pref: &ProposalRef,
    opts: &LandOptions,
) -> Result<Option<ApprovalEvidence>, RdyError> {
    let Some(relay) = &opts.relay else {
        return Ok(opts.evidence.clone());
    };
    if !s.is_human() {
        return Err(RdyError::new(
            E_PERMISSION,
            "only a human session records a relayed approval",
            "run rdy approve under the relaying agent's RDY_ACTOR, with --relayed-by naming that agent",
        ));
    }
    let exact = (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        || (text.starts_with("P-") && text.contains('@'));
    if !exact {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "a relayed approval names the exact version it relays",
            format!("approve {pref} (the version the human was shown)"),
        ));
    }
    if relay.evidence.quoted.trim().is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the evidence quote is empty",
            "quote the human's words with --evidence",
        ));
    }
    // a new relay's agent: the actor, then a run, each in its form (D-ME2C0, D-PWB82); a stored
    // landing is never checked here
    crate::identity::validate_actor(&relay.by.actor)?;
    if let Some(run) = &relay.by.run {
        crate::identity::validate_run(run)?;
    }
    if relay.evidence.session_ref.trim().is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the session reference is empty",
            "pass --session <ref>, or leave it out (it defaults to cli)",
        ));
    }
    match &opts.evidence {
        Some(e)
            if !(e.kind == EvidenceKind::Text
                || (e.kind == EvidenceKind::Card && e.card_hash.is_some())) =>
        {
            Err(RdyError::new(
                E_BAD_INPUT,
                "only a text relay is recorded from the command line",
                "quote the human's words with --evidence",
            ))
        }
        Some(e) => Ok(Some(e.clone())),
        None => Ok(Some(ApprovalEvidence {
            kind: EvidenceKind::Text,
            options: vec![],
            choice: None,
            card_hash: None,
        })),
    }
}

/// `h`'s proposal reference, as the approver saw it: `P-<stable short>@<n>`.
fn version_ref(st: &ReadState, h: &Hash) -> Result<ProposalRef, RdyError> {
    let p = st.reader().proposal(h)?;
    Ok(ProposalRef {
        version: *h,
        stable: p.stable.unwrap_or(*h),
        n: version_number(st, h)?,
    })
}

/// Resolves a proposal reference on `st`: a version hash, `P-<prefix>` (the latest version) or
/// `P-<prefix>@<n>`.
pub(crate) fn resolve_in(st: &ReadState, text: &str) -> Result<ProposalRef, RdyError> {
    let reader = st.reader();
    if let Ok(h) = text.parse::<Hash>() {
        if !reader.has_kind(rdy_store::kinds::PROPOSAL, &h)? {
            if reader.has(&h)? {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("{text} is not a proposal"),
                    "pass the id returned by rdy_propose",
                ));
            }
            return Err(rdy_store::StoreError::NotFound(h).into());
        }
        let p = reader.proposal(&h)?;
        return Ok(ProposalRef {
            version: h,
            stable: p.stable.unwrap_or(h),
            n: version_number(st, &h)?,
        });
    }
    let parsed = text.strip_prefix("P-").and_then(|rest| {
        let (prefix, n) = match rest.split_once('@') {
            Some((prefix, n)) => (prefix, Some(n.parse::<u32>().ok()?)),
            None => (rest, None),
        };
        ((4..=64).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| (prefix.to_ascii_lowercase(), n))
    });
    let Some((prefix, want)) = parsed else {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{:?} is not a proposal id", crate::views::clip(text)),
            "pass P-<id> or P-<id>@<n> from rdy_propose, or a version hash",
        ));
    };
    let found = reader.refs_with_prefix(&format!("proposal/{prefix}"))?;
    let (name, value) = match found.as_slice() {
        [] => {
            return Err(RdyError::new(
                E_NOT_FOUND,
                format!("no proposal P-{prefix}"),
                "list proposals with rdy log (or the id rdy_propose returned)",
            ));
        }
        [one] => one,
        many => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("P-{prefix} is ambiguous ({} proposals)", many.len()),
                "use more characters",
            ));
        }
    };
    let stable: Hash = name["proposal/".len()..]
        .parse()
        .map_err(|_| rdy_store::StoreError::Decode(format!("bad proposal ref {name:?}")))?;
    let latest = ref_hash(value, "a proposal ref")?;
    let latest_n = version_number(st, &latest)?;
    let Some(n) = want else {
        return Ok(ProposalRef {
            version: latest,
            stable,
            n: latest_n,
        });
    };
    if n == 0 || n > latest_n {
        let short = stable.short();
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("P-{short} has {latest_n} version(s)"),
            format!("approve P-{short}@{latest_n}"),
        ));
    }
    let mut version = latest;
    for _ in 0..latest_n - n {
        version = reader.proposal(&version)?.prev.ok_or_else(|| {
            rdy_store::StoreError::Decode(format!("proposal {version} has no previous version"))
        })?;
    }
    Ok(ProposalRef { version, stable, n })
}

/// Every block id `b` points at: inline references and link targets.
/// The earlier of two points on canon's chain (`None` is before the first landing): `a`, unless
/// `b` is `a` or one of its ancestors.
fn earlier_landing(
    st: &ReadState,
    a: Option<Hash>,
    b: Option<Hash>,
) -> Result<Option<Hash>, RdyError> {
    let Some(b) = b else { return Ok(None) };
    let mut cur = a;
    while let Some(h) = cur {
        if h == b {
            return Ok(Some(b));
        }
        cur = st.reader().landing(&h)?.prev;
    }
    Ok(a)
}

fn targets(b: &Block) -> BTreeSet<BlockId> {
    b.ref_tokens()
        .iter()
        .filter_map(|t| BlockId::parse(t).ok())
        .chain(b.links.values().flatten().map(|l| l.target.clone()))
        .collect()
}

pub(crate) fn changes_of(st: &ReadState, ids: &[Hash]) -> Result<Vec<Change>, RdyError> {
    ids.iter()
        .map(|h| st.reader().change(h).map_err(RdyError::from))
        .collect()
}

/// The code of the warning for a decision a proposal lands with nothing to apply it.
const W_UNAPPLIED_DECISION: &str = "W_UNAPPLIED_DECISION";

/// cc16 (FW-CH5, D-2YCVK): one `W_UNAPPLIED_DECISION` warning per decision `changes` create that
/// no content applies in `view` and that does not say `needs_content: false`. A warning never
/// refuses the proposal.
fn unapplied_warnings(
    st: &ReadState,
    view: &str,
    changes: &[Change],
) -> Result<Vec<Issue>, RdyError> {
    let created: Vec<&BlockId> = changes
        .iter()
        .flat_map(|c| &c.ops)
        .filter_map(|o| match o {
            Op::Create { block } if block.block_type == decision::DECISION => Some(&block.id),
            _ => None,
        })
        .collect();
    if created.is_empty() {
        return Ok(vec![]);
    }
    let scope = Scope::View(view.to_string());
    let (m, g) = (st.materialized(&scope)?, st.graph(&scope)?);
    Ok(created
        .into_iter()
        .filter_map(|id| m.get(id))
        .filter(|b| crate::coverage::lands_unapplied(&m, &g, st.pack(), b))
        .map(|b| Issue {
            severity: Severity::Warning,
            code: W_UNAPPLIED_DECISION,
            block: Some(b.id.clone()),
            message: format!("decision {} lands with no content that applies it", b.id),
            fix: format!(
                "write the content that carries it with implements: {}, or link that content to it with decided_by; for a process ruling, set needs_content: false",
                b.id
            ),
        })
        .collect())
}

/// The landing that already carried any of `p`'s changes into canon: one index lookup per
/// change (lidx1).
pub(crate) fn landed_as(st: &ReadState, p: &Proposal) -> Result<Option<Hash>, RdyError> {
    for c in &p.changes {
        if let Some(l) = st.reader().landing_of(c)? {
            return Ok(Some(l));
        }
    }
    Ok(None)
}

/// Whether proposal version `version` (`p`) is withdrawn (D-K0SS1): its view is closed now, or
/// the view was closed when the log took the version's `PROPOSAL` frame (a record from before
/// R2), or the view's chain took a close after that frame (`closed_after`, from the head this
/// state holds). An undo of the close reopens the view, never the version. The caller
/// has checked that the version is its proposal's latest and has not landed. A view with no head
/// in the state is not closed, and its versions are not withdrawn.
pub(crate) fn withdrawn(st: &ReadState, version: &Hash, p: &Proposal) -> Result<bool, RdyError> {
    let Some(head) = st.view_head(&p.view) else {
        return Ok(false);
    };
    if head.closed {
        return Ok(true);
    }
    Ok(st
        .reader()
        .closed_after(&p.view, &head.head, rdy_store::kinds::PROPOSAL, version)?)
}

/// The refusal of an approval, a landing or an advice acceptance of version `pref`, which a close
/// of view `view` withdrew (D-K0SS1).
pub(crate) fn withdrawn_error(pref: &ProposalRef, view: &str) -> RdyError {
    RdyError::new(
        E_VIEW_CLOSED,
        format!("{pref} was withdrawn when view {view:?} closed"),
        format!(
            "reopen the view with rdy undo --view {view} <close op> (rdy view show {view} lists the ops), then propose again"
        ),
    )
}

/// The latest version of view `view`'s open proposal, read from its one `open/<view>` ref; `None`
/// when the view has none, or its latest version has landed or is withdrawn (D-K0SS1: after an
/// undo of the close, the old version does not carry the view's changes). It reads no other
/// proposal and counts no versions, so its cost does not grow with the proposals of other views
/// or with history.
pub(crate) fn open_proposal_of_view(
    st: &ReadState,
    view: &str,
) -> Result<Option<Proposal>, RdyError> {
    let Some(b) = st.reader().get_ref(&format!("open/{view}"))? else {
        return Ok(None);
    };
    if b.len() != 32 {
        return Ok(None);
    }
    let stable = ref_hash(&b, "an open ref")?;
    let latest = latest_version(st, &stable)?;
    let p = st.reader().proposal(&latest)?;
    if landed_as(st, &p)?.is_some() || withdrawn(st, &latest, &p)? {
        return Ok(None);
    }
    Ok(Some(p))
}

/// Every open proposal at its latest version: each `open/<view>` ref whose latest version has
/// not landed and is not withdrawn (D-K0SS1), sorted by the ref's text (`P-<stable short>@<n>`).
/// It reads the store's open proposal refs only, never the empty ref each landed view leaves
/// (final review fix round 1, R5), so its cost follows the open proposals, not the project's
/// history.
pub(crate) fn open_proposals(st: &ReadState) -> Result<Vec<(ProposalRef, Proposal)>, RdyError> {
    let mut open = vec![];
    for (_, b) in st.reader().open_proposal_refs()? {
        if b.len() != 32 {
            continue;
        }
        let stable = ref_hash(&b, "an open ref")?;
        let latest = latest_version(st, &stable)?;
        let p = st.reader().proposal(&latest)?;
        if landed_as(st, &p)?.is_some() || withdrawn(st, &latest, &p)? {
            continue;
        }
        let pref = ProposalRef {
            version: latest,
            stable,
            n: version_number(st, &latest)?,
        };
        open.push((pref, p));
    }
    open.sort_by_cached_key(|(pref, _)| pref.to_string());
    Ok(open)
}

/// The landing checks of version `pref` (`p`, with its changes `changes` read), computed on a
/// published state: checks 1 to 3 of the plan's closed readiness list ("Readiness before M3b",
/// D-3JZP4). The queue and every approval path read them (D-ME2HD). Each failed check is one
/// issue with its own code, message and fix:
/// 1. a conflict on a block the version writes (`gconf1`, `E_CONFLICT_UNRESOLVED`);
/// 2. a blocking tension the landing would put into canon (`tgat1`, `E_BLOCKING_TENSION`);
/// 3. an error in the canon the landing would give (`E_VALIDATION`, or the merge's refusal).
///
/// Each check that can be made is made, so the agent gets every fix at once (fix round 1). A
/// version with a conflict still gets check 2, less the
/// tensions whose blocks all carry the conflict (the conflict reports them, as
/// `proposal_tensions` leaves them out). Check 3 is not made for it: the canon its landing would
/// give depends on how the conflict is resolved, and the merge onto canon refuses for the same
/// cause. A key rdy derives again at the landing (K1) is not a clash. Beside the issues: the
/// blocks that carry a blocking conflict (the `conflicted` of `proposal_tensions`).
pub(crate) fn landing_readiness(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
    changes: &[Change],
) -> Result<(Vec<Issue>, BTreeSet<BlockId>), RdyError> {
    let as_issue = |e: RdyError| Issue {
        severity: Severity::Error,
        code: e.code,
        block: None,
        message: e.message,
        fix: e.fix,
    };
    let mut out = vec![];
    let blocking = blocking_conflicts(st, &p.view, &p.changes)?;
    let conflicted: BTreeSet<BlockId> = blocking.iter().map(|c| c.block.clone()).collect();
    if !blocking.is_empty() {
        out.push(as_issue(unresolved(st, &p.view, &blocking)?));
    }
    let applied = st.landing_order(changes);
    let landing = st.landing_tensions_applied(&p.view, &p.changes, changes, &applied)?;
    let mut cache = crate::tensions::info::InfoCache::default();
    let refusing: Vec<TensionInfo> = st
        .refusing_among(&p.changes, landing)
        .iter()
        .filter(|t| conflicted.is_empty() || !t.blocks().iter().all(|b| conflicted.contains(b)))
        .map(|t| st.tension_info_with(t, &mut cache))
        .collect();
    if !refusing.is_empty() {
        out.push(as_issue(blocking_tensions(pref, &refusing)));
    }
    if !conflicted.is_empty() {
        return Ok((out, conflicted));
    }
    match prospective(st, changes, Some(&p.view)) {
        Ok((next, applied)) => {
            let canon = st.materialized(&Scope::Canon)?;
            let vm = st.materialized(&Scope::View(p.view.clone()))?;
            let rekeyed: BTreeSet<BlockId> =
                crate::keys::rederive(&next, &rekeyable_in(changes, &applied, &canon), &vm)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
            let issues = prospective_issues_of(st, changes)?;
            let first = issues.iter().find(|i| {
                i.severity == Severity::Error
                    && !(i.code == E_KEY_CLASH
                        && i.block.as_ref().is_some_and(|b| rekeyed.contains(b)))
            });
            if let Some(first) = first {
                out.push(as_issue(RdyError::new(
                    E_VALIDATION,
                    first.message.clone(),
                    first.fix.clone(),
                )));
            }
        }
        // the merge's refusals, worded for the approver as the landing words them
        Err(e) if [E_STALE_BASE, E_ALREADY_EXISTS, E_FROZEN].contains(&e.code) => {
            out.push(as_issue(e));
        }
        Err(e) => return Err(e),
    }
    Ok((out, conflicted))
}

/// The conflicts the changes `changes` of view `view` carry on the blocks they write: their
/// merge onto the view's effective base, on those blocks only. For the view's whole set these
/// are the view's conflicts on the blocks it writes; a subset leaves out conflicts it does
/// not carry; a set without the view's later resolution still has the conflict.
pub(crate) fn blocking_conflicts(
    st: &ReadState,
    view: &str,
    changes: &[Hash],
) -> Result<Vec<Conflict>, RdyError> {
    let reader = st.reader();
    let base = st.effective_base(view);
    let state = reader.state_of(base)?;
    let cs = changes_of(st, changes)?;
    let touched: BTreeSet<BlockId> = cs
        .iter()
        .flat_map(|c| c.ops.iter().map(|op| op.block_id().clone()))
        .collect();
    let input: Vec<MergeChange<'_>> = cs
        .iter()
        .zip(changes)
        .map(|(c, id)| MergeChange {
            id: *id,
            deps: &c.deps,
            ops: &c.ops,
            is_replay: c.links.replayed_from.is_some(),
        })
        .collect();
    let deleters = reader.merge_deleters(base.as_ref(), &state, &touched)?;
    // the merge asks for the producer of each touched block's base version only: read them
    // first, so a damaged index entry fails the check instead of reading as "no producer",
    // which would settle the base's side of a conflict (msw2)
    let mut producers: HashMap<Hash, Option<Hash>> = HashMap::new();
    for v in touched.iter().filter_map(|b| state.blocks.get(b)) {
        if !producers.contains_key(v) {
            producers.insert(*v, reader.producer(v)?);
        }
    }
    let order_only = reader.order_only_pairs(&state, cs.iter().flat_map(|c| &c.ops));
    Ok(merge_on_blocks(
        &state,
        &input,
        &|v| producers.get(v).copied().flatten(),
        &|b| deleters.get(b).copied(),
        &|v, op| order_only.holds(v, op),
        &touched,
    )
    .conflicts)
}

/// `E_CONFLICT_UNRESOLVED` for the conflicts a version carries on the blocks it writes: one
/// issue per conflict, naming its sides (`canon` for a landed side).
fn unresolved(st: &ReadState, view: &str, blocking: &[Conflict]) -> Result<RdyError, RdyError> {
    let blocks = blocking
        .iter()
        .map(|c| c.block.to_string())
        .collect::<Vec<_>>();
    let mut e = RdyError::new(
        E_CONFLICT_UNRESOLVED,
        format!(
            "{} unresolved conflict(s) on blocks this proposal writes: {}",
            blocking.len(),
            blocks.join(", ")
        ),
        format!(
            "write each block in view {view:?} to resolve it (base = the conflict's base or one of its sides), then propose again and approve the new version"
        ),
    );
    for c in blocking {
        let mut sides = vec![];
        for side in &c.sides {
            sides.push(match side.change {
                Some(ch) if st.reader().landing_of(&ch)?.is_none() => ch.short(),
                _ => "canon".to_string(),
            });
        }
        e.issues.push(Issue {
            severity: Severity::Error,
            code: E_CONFLICT_UNRESOLVED,
            block: Some(c.block.clone()),
            message: format!(
                "{} has {} sides: {}",
                c.block,
                c.sides.len(),
                sides.join(", ")
            ),
            fix: format!("rdy_context {} in view {view} shows the sides", c.block),
        });
    }
    Ok(e)
}

/// The most tensions a proposal lists; the rest are counted in `tensions_omitted`.
pub const PROPOSAL_TENSIONS_MAX: usize = 50;

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl ReadState {
    /// Every open proposal at its latest version, sorted by the ref's text (see
    /// [`open_proposals`]).
    pub fn open_proposals(&self) -> Result<Vec<(ProposalRef, Proposal)>, RdyError> {
        open_proposals(self)
    }

    /// The tensions that refuse landing `changes` (one version's changes, proposed from view
    /// `view`), of `tensions` (their [`ReadState::landing_tensions`]): each blocking, not
    /// dismissed, and with every side canon or one of `changes`, so that landing them would put
    /// it into canon. By id.
    ///
    /// The gate judges what lands (tgat1): a change that another view also holds, or a write the
    /// view overwrites later but a subset leaves out, is read as the landing set has it, not as
    /// the in-flight set does. A tension whose sides are all canon (a fork, duplicate definition
    /// or deprecated reference standing in canon; FW-CH6, D-CRTJG) has no change side, so it
    /// never refuses a landing.
    fn refusing_among(&self, changes: &[Hash], tensions: Vec<Arc<Tension>>) -> Vec<Arc<Tension>> {
        let mine: BTreeSet<&Hash> = changes.iter().collect();
        tensions
            .into_iter()
            .filter(|t| {
                t.severity == TensionSeverity::Blocking
                    && self.tension_status(t) == TensionStatus::Open
                    && t.sides.iter().all(|s| match s {
                        SideRef::Canon { .. } => true,
                        SideRef::Change(h) => mine.contains(h),
                        SideRef::Intent { .. } => false,
                    })
            })
            .collect()
    }

    /// The tensions a version of view `view` with changes `changes` lists: the event-derived
    /// tensions that involve one of `changes` (informational), and the refusing ones
    /// (`refusing_among` the landing forms), each with `refuses` (true exactly for the refusing
    /// ones, so it agrees with what landing does). A kind judged on the net change
    /// (unjustified_change, FW-JU1) is listed only in its landing form, never in another view's
    /// in-flight form. The refusing ones come first, then by id. A
    /// tension whose involved blocks all carry one of the version's blocking conflicts
    /// (`conflicted`) is left out: the conflict reports it. At most `max` are listed, built
    /// with their sides; the rest are only counted (the second value).
    pub fn proposal_tensions(
        &self,
        view: &str,
        changes: &[Hash],
        conflicted: &BTreeSet<BlockId>,
        max: usize,
    ) -> Result<(Vec<TensionInfo>, usize), RdyError> {
        let listed = self.listed_tensions(view, changes, conflicted, None)?;
        let omitted = listed.len().saturating_sub(max);
        let mut cache = crate::tensions::info::InfoCache::default();
        let out = listed
            .into_iter()
            .take(max)
            .map(|(r, t)| {
                let mut info = self.tension_info_with(&t, &mut cache);
                info.refuses = Some(r);
                info
            })
            .collect();
        Ok((out, omitted))
    }

    /// The tensions [`ReadState::proposal_tensions`] lists, in its order, each with `refuses`,
    /// none built: every one, uncut. With `fixed` (the tension set a stored version was made
    /// with, `plan::compute::fixed_tensions`), an in-flight form it does not list is left out
    /// (ruling task-8 F1): a later write's tension is context only.
    pub(crate) fn listed_tensions(
        &self,
        view: &str,
        changes: &[Hash],
        conflicted: &BTreeSet<BlockId>,
        fixed: Option<&BTreeSet<Hash>>,
    ) -> Result<Vec<(bool, Arc<Tension>)>, RdyError> {
        let landing = self.landing_tensions(view, changes)?;
        let net = |t: &Tension| crate::tensions::judged_on_the_net_change(t.kind);
        let net_forms: Vec<Arc<Tension>> = landing.iter().filter(|t| net(t)).cloned().collect();
        let refusing = self.refusing_among(changes, landing);
        let refuses: BTreeSet<Hash> = refusing.iter().map(|t| t.id).collect();
        // never the whole set: a proposal made right after a landing must not build canon's
        // part, which no tension with a change side is in
        // FW-JU1: an unjustified change is judged on the proposal's net change: its landing form, never another view's in-flight form
        let mine = self
            .tensions_of_changes(changes)?
            .into_iter()
            .filter(|t| !net(t) && fixed.is_none_or(|f| f.contains(&t.id)));
        let mut seen = BTreeSet::new();
        let mut listed = vec![];
        // a refusing tension is listed in its landing form; an in-flight form with another id
        // (another severity, sides or versions) is listed beside it as information
        let all = refusing.into_iter().chain(net_forms).chain(mine);
        for t in all {
            if !seen.insert(t.id) {
                continue;
            }
            if !conflicted.is_empty() && t.blocks().iter().all(|b| conflicted.contains(b)) {
                continue;
            }
            listed.push((refuses.contains(&t.id), t));
        }
        // the order needs only the ids (hex in a fixed width sorts as the bytes do), so only the
        // listed ones are built
        listed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        Ok(listed)
    }
}

/// `E_POLICY_HUMAN` for version `pref` with `reasons` (O14).
fn policy_human(pref: &ProposalRef, reasons: &[String]) -> RdyError {
    RdyError::new(
        E_POLICY_HUMAN,
        format!(
            "the authority policy needs a person for {pref}: {}",
            reasons.join("; ")
        ),
        POLICY_HUMAN_FIX,
    )
}

/// O14, O6: `rdy_land` reads the verdict assessment `a` stored for version `pref` and computes
/// none. A version the policy does not accept (or an assessment with no verdict) is
/// `E_POLICY_HUMAN` with its reasons; one it accepts needs the session's actor to match the
/// accepting rule's `actors`, else `E_POLICY_HUMAN` naming the actor. Returns the live policy's
/// ref, which freshness (O16 (1)) has checked is the assessment's.
fn policy_acceptance(
    st: &ReadState,
    pref: &ProposalRef,
    s: &Session,
    a: &rdy_model::assessment::Assessment,
) -> Result<rdy_model::assessment::PolicyRef, RdyError> {
    use rdy_model::assessment::PolicyVerdict;
    let rule = match a.policy.as_ref().map(|p| &p.verdict) {
        Some(PolicyVerdict::Accepts { rule }) => rule,
        Some(PolicyVerdict::NeedsPerson { reasons, .. }) => {
            return Err(policy_human(pref, reasons));
        }
        None => {
            return Err(policy_human(
                pref,
                &[crate::policy_verdict::NO_POLICY.to_string()],
            ));
        }
    };
    let canon = st.materialized(&Scope::Canon)?;
    let bug = || {
        RdyError::new(
            E_INTERNAL,
            format!(
                "the stored verdict of {pref} names rule {rule}, which the live authority policy does not hold"
            ),
            "report this as a bug in rdy",
        )
    };
    let live = crate::policy::live_policy(st, &canon).ok_or_else(bug)?;
    let accepting = live
        .fields
        .as_ref()
        .and_then(|f| f.rules.iter().find(|r| &r.name == rule))
        .ok_or_else(bug)?;
    if !accepting
        .actors
        .iter()
        .any(|pattern| rdy_schema::policy::pattern_matches(pattern, &s.actor))
    {
        return Err(policy_human(
            pref,
            &[format!(
                "actor {} matches none of rule {}'s actors",
                s.actor, accepting.name
            )],
        ));
    }
    Ok(live.reference())
}

/// M3c O16's freshness checks of version `pref` (record `p`) against its assessment `a`, after
/// M3b's: the message of the first that fails, or None. `since` is the landings since `a.base`,
/// newest first. (1) The live policy (canon's at its head) is not the one `a` was judged under.
/// (2) Under `coverage.gate: no_lower`, a landing since the base wrote a block of one of `a`'s
/// pages (the page's blocks in canon now), oldest landing first, then the pages in `a`'s order.
/// (3) For [`Lander::Policy`] only, under an accepting rule with `lifecycle: draft`: a block
/// the version writes is in stable content in canon now, in id order.
fn policy_moved(
    st: &ReadState,
    pref: &ProposalRef,
    p: &Proposal,
    a: &rdy_model::assessment::Assessment,
    since: &[(Hash, LandingRecord)],
    lander: Lander,
) -> Result<Option<String>, RdyError> {
    let canon = st.materialized(&Scope::Canon)?;
    let live = crate::policy::live_policy(st, &canon);
    let assessed = a.policy.as_ref().and_then(|ap| ap.policy.clone());
    if live.as_ref().map(crate::policy::LivePolicy::reference) != assessed {
        return Ok(Some(format!(
            "proposal {pref} moved: the authority policy changed after its assessment"
        )));
    }
    if st.coverage_gate() == crate::CoverageGate::NoLower && !since.is_empty() {
        let pages: Vec<(&BlockId, BTreeSet<BlockId>)> = a
            .coverage
            .pages
            .iter()
            .map(|g| {
                let blocks = crate::coverage::page_blocks(&canon, &g.doc);
                (&g.doc, blocks.into_iter().collect())
            })
            .collect();
        for (l, rec) in since.iter().rev() {
            let mut written = BTreeSet::new();
            for c in &rec.changes {
                for op in &st.reader().change(c)?.ops {
                    written.insert(op.block_id().clone());
                }
            }
            if let Some((doc, _)) = pages
                .iter()
                .find(|(_, blocks)| blocks.iter().any(|b| written.contains(b)))
            {
                let path = canon.path_of(doc).unwrap_or_else(|| doc.to_string());
                return Ok(Some(format!(
                    "proposal {pref} moved: landing {} changed page {path} after its assessment",
                    l.short()
                )));
            }
        }
    }
    if lander == Lander::Policy
        && let Some(rdy_model::assessment::PolicyVerdict::Accepts { rule }) =
            a.policy.as_ref().map(|ap| &ap.verdict)
        && live
            .as_ref()
            .and_then(|l| l.fields.as_ref())
            .and_then(|f| f.rules.iter().find(|r| &r.name == rule))
            .is_some_and(|r| r.lifecycle == rdy_schema::policy::Lifecycle::Draft)
    {
        let mut written = BTreeSet::new();
        for c in &p.changes {
            for op in &st.reader().change(c)?.ops {
                written.insert(op.block_id().clone());
            }
        }
        if let Some(id) = written
            .iter()
            .find(|id| crate::brief::effective_stable(&canon, Some(id)))
        {
            return Ok(Some(format!(
                "proposal {pref} moved: block {id} is in stable content now"
            )));
        }
    }
    Ok(None)
}

/// `rdy_land` and the daemon's `land` op (D-ZWP7Q, O14): [`Rdy::land_by_policy`] for session
/// `s`, under the writer. The reply is the approval's: the `LandInfo`, `signatures` (always
/// empty: a policy landing is never signed, D-DKAMT) and the consequences of the version it
/// landed.
pub fn run_land(
    engine: &dyn crate::engine::Engine,
    s: &Session,
    proposal: String,
) -> Result<serde_json::Value, RdyError> {
    let s = s.clone();
    let mut v = crate::engine::write_typed(engine, move |r| r.land_by_policy(&s, &proposal))?;
    v["signatures"] = serde_json::json!([]);
    crate::consequence::report_approval(engine, &mut v);
    Ok(v)
}

/// D-ME2HD: the refusal of an approval of version `pref`, which is not ready. Its issues are the
/// version's readiness issues, in the queue's order, each with its own code and fix
/// ([`crate::readiness::readiness`]); none is added, dropped, merged or re-worded.
/// The message counts them (one issue reads `1 issue`, more read `<n> issues`; TASK-548). No card.
pub(crate) fn proposal_not_ready(pref: &ProposalRef, issues: Vec<Issue>) -> RdyError {
    let mut e = RdyError::new(
        E_PROPOSAL_NOT_READY,
        format!(
            "{pref} is not ready: {} issue{}",
            issues.len(),
            if issues.len() == 1 { "" } else { "s" }
        ),
        "fix each issue and propose again: rdy_propose / rdy propose",
    );
    e.issues = issues;
    e
}

/// O10: the issue of a version whose readiness passes but that has no stored assessment.
pub(crate) fn no_stored_assessment(pref: &ProposalRef) -> Issue {
    Issue {
        severity: Severity::Error,
        code: E_VALIDATION,
        block: None,
        message: format!("{pref} has no stored assessment"),
        fix: "propose it again: rdy_propose / rdy propose".into(),
    }
}

/// The fix of a failed freshness check whose fresh readiness fails too (O11).
const O11_FIX: &str = "fix each issue and propose again: rdy_propose / rdy propose";

/// The decisions the later refusals read (I8, I-c), from the ops of `changes` alone, in the
/// order the merge applies them: for each decision block, its last `Create` or `Update` op
/// (none when a later op deletes it). A block whose status is `proposed` is a proposed
/// decision, `(id, <id>@<short of its version>)`; any other whose `reaffirmed_causes` against
/// canon's version is not empty is a re-affirmed decision, `(id, ref, one_way)`.
#[allow(clippy::type_complexity)]
fn own_decisions(
    st: &ReadState,
    changes: &[Change],
) -> Result<(Vec<(BlockId, String)>, Vec<(BlockId, String, bool)>), RdyError> {
    let applied = st.landing_order(changes);
    let mut last: BTreeMap<BlockId, Option<Block>> = BTreeMap::new();
    for op in applied_ops(changes, &applied) {
        match op {
            Op::Create { block } | Op::Update { block, .. }
                if block.block_type == decision::DECISION =>
            {
                last.insert(block.id.clone(), Some(block));
            }
            op => {
                if let Some(b) = last.get_mut(op.block_id()) {
                    *b = None;
                }
            }
        }
    }
    let canon = st.materialized(&Scope::Canon)?;
    let (mut decisions, mut reaffirms) = (vec![], vec![]);
    for (id, b) in last {
        let Some(b) = b else { continue };
        let r = format!("{id}@{}", b.version_hash().short());
        if decision::status(&b) == Some(decision::DecisionStatus::Proposed) {
            decisions.push((id, r));
        } else if !rdy_model::engine_fields::reaffirmed_causes(canon.get(&id), &b).is_empty() {
            let one_way = b.field_str("reversibility") == Some("one_way");
            reaffirms.push((id, r, one_way));
        }
    }
    Ok((decisions, reaffirms))
}

/// F2 (D-DKJS5): the refusal of an approval while exceptions of `pref` have no confirm answer. It
/// names each exception and carries its card in `corrected.exceptions` (O21). A version with no
/// summary never gets here: readiness refuses it first (D-ME2HD).
fn exceptions_open(pref: &ProposalRef, open: &[Item]) -> RdyError {
    let refs: Vec<&str> = open.iter().map(|i| i.r#ref.as_str()).collect();
    let cards: Vec<_> = open.iter().map(card_of).collect();
    RdyError::new(
        E_EXCEPTION_OPEN,
        format!("{} exceptions of {pref} have no answer", open.len()),
        format!("answer each exception card first: {}", refs.join(", ")),
    )
    .with_corrected(serde_json::json!({ "exceptions": cards }))
}

/// D-AAQ9Z: the refusal of an approval while the deferral card of `pref` (`card`, listing `n`
/// deferrals) has no answer that accepts it. It carries the card in `corrected.deferral`.
fn deferrals_open(pref: &ProposalRef, card: &Item, n: usize) -> RdyError {
    RdyError::new(
        E_DEFERRAL_OPEN,
        format!("{n} deferrals of {pref} have no answer"),
        format!("answer the deferral card first: {}", card.r#ref),
    )
    .with_corrected(serde_json::json!({ "deferral": card_of(card) }))
}

/// `E_BLOCKING_TENSION` for the refusing tensions of version `pref`: one issue per tension. The
/// fix names what resolves each refusing kind ([`crate::tensions::resolve_hint`]), each distinct
/// hint once, in the order the tensions come.
pub(crate) fn blocking_tensions(pref: &ProposalRef, refusing: &[TensionInfo]) -> RdyError {
    let mut hints: Vec<&str> = vec![];
    for t in refusing {
        let h = crate::tensions::resolve_hint(t.kind);
        if !hints.contains(&h) {
            hints.push(h);
        }
    }
    if hints.is_empty() {
        hints.push(crate::tensions::RESOLVE_HINT);
    }
    let mut e = RdyError::new(
        E_BLOCKING_TENSION,
        format!(
            "landing {pref} would put {} blocking tension(s) into canon",
            refusing.len()
        ),
        format!(
            "{} (rdy resolve <tn-…> --dismiss <reason>; a reviewer of another actor or a human); rdy tensions --involving <change> lists them, a form only this landing gives with its proposal",
            hints.join("; ")
        ),
    );
    e.issues = refusing
        .iter()
        .map(|t| Issue {
            severity: Severity::Error,
            code: E_BLOCKING_TENSION,
            // the first involved block: sides are sorted, and each side's blocks in order
            block: t
                .sides
                .first()
                .and_then(|side| side.blocks.first())
                .and_then(|b| BlockId::parse(&b.block).ok()),
            message: format!("{} {}: {}", t.id, t.kind, t.summary),
            fix: match crate::tensions::resolve_hint(t.kind) {
                crate::tensions::RESOLVE_HINT => format!("resolve, repin or dismiss {}", t.id),
                h => format!("{}: {h}", t.id),
            },
        })
        .collect();
    e
}

/// `open`, `chosen` or `abandoned`.
fn alt_status(s: &AltStatus) -> &'static str {
    match s {
        AltStatus::Open => "open",
        AltStatus::Chosen { .. } => "chosen",
        AltStatus::Abandoned { .. } => "abandoned",
    }
}

/// View `view`'s label in `set`, or the view's name when it is not a member.
fn member_label<'a>(set: &'a AltSet, view: &'a str) -> &'a str {
    set.members
        .iter()
        .find(|m| m.view == view)
        .map_or(view, |m| m.label.as_str())
}

/// altg1, the member rule at propose: a view a settled set did not choose proposes nothing, with
/// or without a pick; a member view of an open set names its own member, and a pick names an open
/// set the view is a member of, and the view's own member (by label or view name). Returns the
/// choice the version stores.
fn proposed_alt(
    st: &ReadState,
    view: &str,
    alternative: Option<&AltPick>,
) -> Result<Option<AltChoice>, RdyError> {
    not_lost(st, view, |label| {
        format!("view {view} is its member {label}, and its work can no longer land")
    })?;
    let Some(pick) = alternative else {
        return match st.alt_of_view(view)? {
            Some((stable, _, set)) => {
                let id = alt_label(&stable);
                Err(RdyError::new(
                    E_ALT_UNCHOSEN,
                    format!(
                        "view {view} is a member of {id} ({}): name the choice",
                        set.title
                    ),
                    format!(
                        "propose with alternatives: {{set: \"{id}\", chosen: \"{}\"}}",
                        member_label(&set, view)
                    ),
                ))
            }
            None => Ok(None),
        };
    };
    let (stable, _, set) = st.resolve_alt(&pick.set)?;
    let id = alt_label(&stable);
    if set.status != AltStatus::Open {
        return Err(RdyError::new(
            E_ALT_MEMBER,
            format!("{id} is {}", alt_status(&set.status)),
            "only an open set takes a choice: propose without alternatives",
        ));
    }
    let Some(own) = set.members.iter().find(|m| m.view == view) else {
        return Err(RdyError::new(
            E_ALT_MEMBER,
            format!("view {view} is not a member of {id}"),
            "propose without alternatives, or from a member view",
        ));
    };
    if pick.chosen != own.label && pick.chosen != own.view {
        let fix = match set
            .members
            .iter()
            .find(|m| m.label == pick.chosen || m.view == pick.chosen)
        {
            Some(m) => format!("propose from {} to choose it", m.view),
            None => format!("name view {view}'s member: chosen: \"{}\"", own.label),
        };
        return Err(RdyError::new(
            E_ALT_MEMBER,
            format!("{id}: {} is not view {view}'s member", pick.chosen),
            fix,
        ));
    }
    Ok(Some(AltChoice {
        set: stable,
        member: view.to_string(),
    }))
}

/// altg1, the gate rule at land: a view a settled set did not choose lands nothing; the set the
/// version chose must still be open with the proposing view a member, and a version that chose
/// nothing must not come from a member view (the view joined a set after it was proposed).
/// Returns what the landing records and closes: the set's latest version, and every other member
/// view.
fn landing_alt(st: &ReadState, p: &Proposal) -> Result<Option<AltLanding>, RdyError> {
    not_lost(st, &p.view, |_| {
        "this proposal can no longer land".to_string()
    })?;
    let Some(a) = &p.alternative else {
        return match st.alt_of_view(&p.view)? {
            Some((stable, _, set)) => Err(RdyError::new(
                E_ALT_UNCHOSEN,
                format!(
                    "view {} is a member of {} ({}): this proposal names no choice",
                    p.view,
                    alt_label(&stable),
                    set.title
                ),
                "propose again with alternatives",
            )),
            None => Ok(None),
        };
    };
    let (stable, version, set) = st.resolve_alt(&a.set.to_string())?;
    let id = alt_label(&stable);
    if set.status != AltStatus::Open {
        return Err(RdyError::new(
            E_ALT_MEMBER,
            format!(
                "{id} is {}; this proposal can no longer land",
                alt_status(&set.status)
            ),
            "propose from a view outside the set",
        ));
    }
    if !set.members.iter().any(|m| m.view == a.member) {
        return Err(RdyError::new(
            E_ALT_MEMBER,
            format!(
                "view {} is no longer a member of {id}; this proposal can no longer land",
                a.member
            ),
            "propose again: from a member view with alternatives, or without them",
        ));
    }
    Ok(Some(AltLanding {
        set: stable,
        prev: version,
        member: p.view.clone(),
        close: set
            .members
            .iter()
            .filter(|m| m.view != p.view)
            .map(|m| m.view.clone())
            .collect(),
    }))
}

/// altg1: a view that a chosen or abandoned set did not choose (the set's latest version lists
/// it) lands nothing that chose nothing: its work is the set's rejected alternative, even when
/// the version was made before the view joined the set. `what` names what is refused, given the
/// view's label in the set.
fn not_lost(st: &ReadState, view: &str, what: impl Fn(&str) -> String) -> Result<(), RdyError> {
    match st.alt_lost_by(view) {
        Some((stable, set)) => Err(RdyError::new(
            E_ALT_MEMBER,
            format!(
                "{} is {}; {}",
                alt_label(&stable),
                alt_status(&set.status),
                what(member_label(&set, view))
            ),
            "propose from a view outside the set",
        )),
        None => Ok(()),
    }
}

/// altg1, msw20 (FW-AL1): the bindings of a proposal's changes.
///
/// 1. **Bindings are fixed when a change enters the set.** A change written in a member view of
///    an open set (its `links.alternative`) is that member's alternative while the set is open
///    and still lists the member. A change that exactly one member held when the set was created
///    stays bound to that member, whatever any view includes or excludes later; one that two or
///    more members held then is common ground; one that no member held then is not bound
///    (`CoordHeads::alt_binds`). A member added later is snapshotted at its join the same way;
///    bindings fixed earlier never change. A removed member releases its bindings (only an open
///    set changes members), and a change it shared stays common ground. Every binding applies:
///    a bound change lands only in a version proposed from its member's view that chose that
///    member.
/// 2. **A chosen set keeps its losers bound.** A change bound to a member the set did not choose
///    (by its link or by rule 1) lands only in a proposal that records a decision superseding or
///    refining a decision the choosing landing created (`E_ALT_CHOSEN`), whichever view proposes.
///    A losing alternative never releases itself: the release must come from a change that has
///    no losing binding.
///    When that landing created no decision, only a decision the proposal creates releases it,
///    in a change that is not itself bound to a losing member: an `Update` never releases, and
///    neither does a loser's own decision. An abandoned set binds nothing.
/// 3. **Two open sets.** A change bound in more than one open set can never land: one proposal
///    chooses in one set only, so the refusal names every set and says to exclude the change.
///
/// Open-set checks run before chosen-set checks.
fn linked_alts(
    st: &ReadState,
    view: &str,
    chosen: Option<&AltChoice>,
    ids: &[Hash],
    changes: &[Change],
) -> Result<(), RdyError> {
    let heads = st.coord_heads();
    let open = &heads.open_alts;
    for (id, c) in ids.iter().zip(changes) {
        // the open-set bindings, in stable-id order: the live link and each set's fixed binding
        let mut bindings: BTreeSet<AltChoice> = BTreeSet::new();
        if let Some(a) = &c.links.alternative
            && open
                .get(&a.set)
                .is_some_and(|(_, set)| set.members.iter().any(|m| m.view == a.member))
        {
            bindings.insert(a.clone());
        }
        for stable in open.keys() {
            if let Some(member) = heads.alt_binds.get(stable).and_then(|b| b.bound.get(id)) {
                bindings.insert(AltChoice {
                    set: *stable,
                    member: member.clone(),
                });
            }
        }
        let Some(broken) = bindings
            .iter()
            .find(|a| !(a.member == view && chosen == Some(*a)))
        else {
            continue;
        };
        let label = |a: &AltChoice| {
            open.get(&a.set)
                .map_or(a.member.as_str(), |(_, set)| member_label(set, &a.member))
                .to_string()
        };
        if bindings.len() == 1 {
            return Err(RdyError::new(
                E_ALT_MEMBER,
                format!(
                    "change {} is {} member {}'s alternative",
                    id.short(),
                    alt_label(&broken.set),
                    label(broken)
                ),
                format!("propose from {} to choose it", broken.member),
            ));
        }
        let named: Vec<String> = bindings
            .iter()
            .map(|a| format!("{} member {}", alt_label(&a.set), label(a)))
            .collect();
        return Err(RdyError::new(
            E_ALT_MEMBER,
            format!(
                "change {} is bound in {} open sets: {}; one proposal chooses in one set only",
                id.short(),
                bindings.len(),
                named.join(", ")
            ),
            format!(
                "exclude {} from view {view} (rdy view exclude) and propose without it; it can land once only one of these sets is open",
                id.short()
            ),
        ));
    }
    // each change's losing bindings in chosen sets: (set, losing member)
    let losers: Vec<Vec<(&Hash, &AltSet, &str)>> = ids
        .iter()
        .zip(changes)
        .map(|(id, c)| {
            let mut out = Vec::new();
            for (stable, (_, set)) in &heads.settled_alts {
                let Some(binds) = heads.alt_binds.get(stable) else {
                    continue;
                };
                let Some(ch) = binds.chosen.as_ref() else {
                    continue;
                };
                let linked = c
                    .links
                    .alternative
                    .as_ref()
                    .filter(|a| a.set == *stable)
                    .map(|a| a.member.as_str());
                let fixed = binds.bound.get(id).map(String::as_str);
                for loser in [linked, fixed].into_iter().flatten() {
                    if loser != ch.member && set.members.iter().any(|m| m.view == loser) {
                        out.push((stable, set, loser));
                    }
                }
            }
            out
        })
        .collect();
    if losers.iter().all(Vec::is_empty) {
        return Ok(());
    }
    // the decisions the proposal writes: read once, only when a chosen-set binding needs them
    let free: Vec<bool> = losers.iter().map(Vec::is_empty).collect();
    let (created, targets) = decisions_written(st, changes, &free);
    for (id, bound) in ids.iter().zip(&losers) {
        for &(stable, set, loser) in bound {
            let ch = heads.alt_binds[stable]
                .chosen
                .as_ref()
                .expect("a losing binding is in a chosen set");
            let ok = if ch.decisions.is_empty() {
                created
            } else {
                ch.decisions.iter().any(|d| targets.contains(d))
            };
            if ok {
                continue;
            }
            let at = format!(
                "change {} belongs to member {}, which lost set {}",
                id.short(),
                member_label(set, loser),
                alt_label(stable)
            );
            return Err(if ch.decisions.is_empty() {
                RdyError::new(
                    E_ALT_CHOSEN,
                    format!(
                        "{at} (chosen by landing {}, which recorded no decision)",
                        ch.landing.short()
                    ),
                    "land it with a new decision that records the revisit",
                )
            } else {
                let d = ch
                    .decisions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                RdyError::new(
                    E_ALT_CHOSEN,
                    format!("{at} (chosen by {d})"),
                    format!("land it with a decision that supersedes or refines {d}"),
                )
            });
        }
    }
    Ok(())
}

/// msw20: whether `changes` create a decision that releases a loser when the choice recorded
/// none (RULING Q1, Group 8), and the blocks the proposal's decisions supersede or refine. One
/// principle covers both: a losing alternative never releases itself. A change marked `free`
/// has no losing binding in a chosen set.
/// The decision blocks are folded in landing order: the last `Create` or `Update` of each block
/// wins, and a `Delete` drops it. A block counts as created only when a `Create` in a free change
/// made it (an `Update`, tracking-only or not, never creates). Its supersede and refine targets
/// count only when a free change wrote its surviving version (a free change's `Update` that adds
/// a refine counts; a loser's own decision never does).
fn decisions_written(
    st: &ReadState,
    changes: &[Change],
    free: &[bool],
) -> (bool, BTreeSet<BlockId>) {
    // per block: the surviving version, whether a free change created it, and whether a free
    // change wrote the surviving version
    let mut last: BTreeMap<&BlockId, (Option<&Block>, bool, bool)> = BTreeMap::new();
    for i in st.landing_order(changes) {
        for op in &changes[i].ops {
            match op {
                Op::Create { block } => {
                    last.insert(&block.id, (Some(block), free[i], free[i]));
                }
                Op::Update { block, .. } => {
                    let e = last.entry(&block.id).or_insert((None, false, false));
                    e.0 = Some(block);
                    e.2 = free[i];
                }
                Op::Delete { id, .. } => {
                    last.insert(id, (None, false, false));
                }
            }
        }
    }
    let decisions: Vec<(&Block, bool, bool)> = last
        .into_values()
        .filter_map(|(b, created, wrote)| b.map(|b| (b, created, wrote)))
        .filter(|(b, _, _)| b.block_type == decision::DECISION)
        .collect();
    let targets = decisions
        .iter()
        .filter(|(_, _, wrote)| *wrote)
        .flat_map(|(b, _, _)| {
            [decision::SUPERSEDES, decision::REFINES]
                .into_iter()
                .filter_map(|k| b.links.get(k))
                .flatten()
                .map(|l| l.target.clone())
        })
        .collect();
    (decisions.iter().any(|(_, created, _)| *created), targets)
}

/// The canon that would result from landing `changes` on `st`'s canon, and each applied change's
/// position in `changes`, in applied order. The merge surfaces E_STALE_BASE, E_ALREADY_EXISTS and
/// E_FROZEN early; on the approve path (`approving`, the version's view) they are worded for the
/// approver. The order and the canon are memoised on `st` by the changes' ids (a refusal is
/// not, so each caller words its own).
pub(crate) fn prospective(
    st: &ReadState,
    changes: &[Change],
    approving: Option<&str>,
) -> Result<(Materialized, Vec<usize>), RdyError> {
    let ids: Vec<Hash> = changes.iter().map(Change::id).collect();
    let applied = match st.memo.order.get(&ids) {
        Some(at) => at,
        None => {
            let (_, applied) = st
                .reader()
                .merge_checked(&*st.canon_snapshot()?, changes)
                .map_err(|e| match approving {
                    Some(view) => approver_stale(e, view),
                    None => e.into(),
                })?;
            let at = Arc::new(applied);
            st.memo.order.put(ids.clone(), Arc::clone(&at));
            at
        }
    };
    let next = st.memo.prospective.get_or_try(ids, || {
        Ok::<_, RdyError>(Arc::new(
            st.materialized(&Scope::Canon)?
                .with(&applied_ops(changes, &applied)),
        ))
    })?;
    Ok((Materialized::clone(&next), applied.to_vec()))
}

/// The issues ([`Rdy::issues_in`]) of the canon [`prospective`] gives for `changes` on `st`,
/// memoised on `st` by the changes' ids: on one state the ids fix that canon and its order.
pub(crate) fn prospective_issues_of(
    st: &ReadState,
    changes: &[Change],
) -> Result<Vec<Issue>, RdyError> {
    let ids: Vec<Hash> = changes.iter().map(Change::id).collect();
    let issues = st.memo.issues.get_or_try(ids, || {
        let (next, applied) = prospective(st, changes, None)?;
        Rdy::issues_in(st, changes, &next, &applied).map(Arc::new)
    })?;
    Ok(issues.to_vec())
}

/// The ids whose key rdy may derive again when `changes` (the changes at `applied`) land on
/// `canon`: the blocks they create, and (kfol1, FW-PR3a) the docs and folders they give another
/// key than canon holds, as a key that followed a move or rename. `keys::rederive` keeps only
/// those whose key is rdy's.
fn rekeyable_in(changes: &[Change], applied: &[usize], canon: &Materialized) -> BTreeSet<BlockId> {
    applied
        .iter()
        .flat_map(|&i| changes[i].ops.iter())
        .filter_map(|op| match op {
            Op::Create { block } => Some(block.id.clone()),
            Op::Update { block, .. }
                if rdy_model::tree::is_tree_type(&block.block_type)
                    && crate::keys::key_of(block)
                        != canon.get(&block.id).and_then(crate::keys::key_of) =>
            {
                Some(block.id.clone())
            }
            _ => None,
        })
        .collect()
}

/// The acceptance change's message: the phrases for the ops it holds (`accepts` decisions,
/// gives new `keys`, gives new `order_keys`), in that order, joined as a list, then ` in {title}`.
fn acceptance_message(accepts: bool, keys: bool, order_keys: bool, title: &str) -> String {
    let what: Vec<&str> = [
        (accepts, "accept decisions"),
        (keys, "give new keys"),
        (order_keys, "give new order keys"),
    ]
    .into_iter()
    .filter_map(|(on, phrase)| on.then_some(phrase))
    .collect();
    let list = match what.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!("{list} in {title}")
}

/// The ops of the changes at `applied` (positions in `changes`), in applied order.
pub(crate) fn applied_ops(changes: &[Change], applied: &[usize]) -> Vec<Op> {
    applied
        .iter()
        .flat_map(|&i| changes[i].ops.iter().cloned())
        .collect()
}

impl Rdy {
    /// Validates the canon that would result from landing `changes` on `st`'s canon; returns the
    /// issues and each applied change's position in `changes`, in applied order. `approving` is
    /// the version's view on the approve path (see `prospective`).
    fn prospective_issues(
        &self,
        st: &ReadState,
        changes: &[Change],
        approving: Option<&str>,
    ) -> Result<(Vec<Issue>, Vec<usize>), RdyError> {
        let (_, applied) = prospective(st, changes, approving)?;
        Ok((prospective_issues_of(st, changes)?, applied))
    }

    /// The issues of `next`, the canon that landing `changes` (applied at `applied`) would give.
    /// It reads no store of its own, so the queue's readiness check runs it on a published state.
    fn issues_in(
        st: &ReadState,
        changes: &[Change],
        next: &Materialized,
        applied: &[usize],
    ) -> Result<Vec<Issue>, RdyError> {
        let mut touched = BTreeSet::new();
        let mut deleted = BTreeSet::new();
        for c in applied.iter().map(|&i| &changes[i]) {
            for op in &c.ops {
                match op {
                    Op::Delete { id, .. } => {
                        deleted.insert(id.clone());
                        touched.remove(id);
                    }
                    other => {
                        touched.insert(other.block_id().clone());
                    }
                }
            }
        }
        // an id deleted and then re-created in this proposal exists in the prospective canon:
        // it is not deleted
        deleted.retain(|id| next.get(id).is_none());
        let mut issues = vec![];
        // two views can each give a different block the same key: index next's keys once, and
        // only when a touched block has a key
        let keys = touched
            .iter()
            .any(|id| next.get(id).and_then(crate::keys::key_of).is_some())
            .then(|| crate::keys::Keys::of(next.iter()));
        for id in &touched {
            let Some(found) = next.get(id) else { continue };
            if let (Some(keys), Some(k)) = (&keys, crate::keys::key_of(found)) {
                for (_, other) in keys.holders(k) {
                    // a pair whose holders are both touched is reported once, lower id first
                    if other == id || (other < id && touched.contains(other)) {
                        continue;
                    }
                    let fix = if touched.contains(other) {
                        format!("change the key of {id} or {other} in this view")
                    } else {
                        format!("change the key of {id} in this view")
                    };
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: E_KEY_CLASH,
                        block: Some(id.clone()),
                        message: format!("{id} and {other} would both have key {k:?} in canon"),
                        fix,
                    });
                }
            }
            // a doc from M1 has no stored name; readers give it one, so the gate doesn't require it
            let named;
            let b =
                if found.block_type == rdy_model::tree::DOC && !found.fields.contains_key("name") {
                    let mut c = found.clone();
                    let n = rdy_model::tree::default_name(c.field_str("title"), &c.id);
                    c.fields.insert("name".into(), FieldValue::Str(n));
                    named = c;
                    &named
                } else {
                    found
                };
            let parent = b.parent.as_ref().and_then(|p| next.get(p));
            issues.extend(validate_prefix(st.pack(), b));
            // the read schema (D-3M81W): every block here is a stored version, and the store refuses
            // any write of a stub, so a stub is a trusted, unchanged version a redaction removed
            // (a view resolved to a landed side that was removed later still lands)
            issues.extend(validate_read_block(st.pack(), b, parent));
            // two views can each be fine alone and clash or loop once both are in canon
            let mut walk = b.parent.clone();
            for _ in 0..=next.len() {
                let Some(p) = walk else { break };
                if &p == id {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_PARENT_CYCLE",
                        block: Some(id.clone()),
                        message: format!(
                            "{id} would be inside itself: its parents loop back to it"
                        ),
                        fix: format!(
                            "move {id} (or one of its parents) to a place outside its own subtree"
                        ),
                    });
                    break;
                }
                walk = next.get(&p).and_then(|x| x.parent.clone());
            }
            if rdy_model::tree::is_tree_type(&b.block_type) {
                let mut depth = next.folder_depth(id);
                if b.block_type == rdy_model::tree::FOLDER {
                    depth += next.folder_height(id);
                }
                if depth > rdy_model::tree::MAX_FOLDER_DEPTH {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_TOO_DEEP",
                        block: Some(id.clone()),
                        message: format!(
                            "{id} would put folders {depth} deep (at most {} allowed)",
                            rdy_model::tree::MAX_FOLDER_DEPTH
                        ),
                        fix: format!(
                            "keep folders at most {} deep: move {id} higher in this view",
                            rdy_model::tree::MAX_FOLDER_DEPTH
                        ),
                    });
                }
            }
            // the derived names under each parent are cached in `next`, so this is a lookup
            if rdy_model::tree::is_tree_type(&b.block_type)
                && let Some(n) = next.name_of(id)
            {
                for other in next.named(b.parent.as_ref(), &n.to_ascii_lowercase()) {
                    if &other != id {
                        issues.push(Issue {
                            severity: Severity::Error,
                            code: "E_PATH_CLASH",
                            block: Some(id.clone()),
                            message: format!("{id} and {other} would both be named {n:?} in the same folder (ignoring case)"),
                            fix: format!("rename {id} in this view"),
                        });
                    }
                }
            }
            // canon may have moved since the view was written: every target must still exist,
            // unless it is a kept citation of a deleted target (p-QMGB4, task-5 R1)
            for target in targets(b) {
                if next.get(&target).is_none()
                    && !deleted.contains(&target)
                    && !rdy_graph::keeps_deleted_citation(b, &target)
                {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_DANGLING_REF",
                        block: Some(b.id.clone()),
                        message: format!("{} references {target}, which does not exist in canon", b.id),
                        fix: format!("remove the reference to {target} from {}, or restore {target} in the same view", b.id),
                    });
                }
            }
        }
        if !deleted.is_empty() {
            for b in next.iter() {
                if let Some(parent) = b.parent.as_ref().filter(|p| deleted.contains(*p)) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_MISSING_PARENT",
                        block: Some(b.id.clone()),
                        message: format!(
                            "{} has parent {parent}, which this proposal deletes",
                            b.id
                        ),
                        fix: format!(
                            "move or delete {} in the same view, or don't delete {parent}",
                            b.id
                        ),
                    });
                }
                for gone in targets(b)
                    .intersection(&deleted)
                    .filter(|t| !rdy_graph::keeps_deleted_citation(b, t))
                {
                    issues.push(Issue {
                        severity: Severity::Error,
                        code: "E_DANGLING_REF",
                        block: Some(b.id.clone()),
                        message: format!("{} still references {gone}, which this proposal deletes", b.id),
                        fix: format!("remove the reference to {gone} from {} in the same view, or don't delete it", b.id),
                    });
                }
            }
        }
        Ok(issues)
    }

    /// Warns when the proposal places blocks (creates or moves) under a parent whose siblings, as
    /// the view's base saw them, have different order keys in canon now: any landed re-key of a
    /// sibling since then counts, whether a rebalance or an ordinary move of one block. The
    /// placed blocks' keys were chosen against the old keys.
    fn rekeyed_warnings(
        &self,
        st: &ReadState,
        view_base: Option<Hash>,
        changes: &[Change],
    ) -> Result<Vec<Issue>, RdyError> {
        let mut parents: BTreeSet<Option<BlockId>> = BTreeSet::new();
        for c in changes {
            for op in &c.ops {
                match op {
                    Op::Create { block } => {
                        parents.insert(block.parent.clone());
                    }
                    Op::Update { base, block } => {
                        let old = st.reader().get_block(base)?;
                        if old.parent != block.parent || old.order != block.order {
                            parents.insert(block.parent.clone());
                        }
                    }
                    Op::Delete { .. } => {}
                }
            }
        }
        if parents.is_empty() {
            return Ok(vec![]);
        }
        let base = match view_base {
            Some(l) => st.reader().snapshot_at(&l)?,
            None => rdy_model::StateSnapshot::default(),
        };
        // canon's children that the base also had, at a different version: (parent, id, base, order)
        let canon = st.materialized(&Scope::Canon)?;
        let mut changed = vec![];
        for p in &parents {
            for sib in canon.children(p.as_ref()) {
                if let Some(h) = base.blocks.get(&sib.id)
                    && *h != sib.version_hash()
                {
                    changed.push((p.clone(), sib.id.clone(), *h, sib.order.clone()));
                }
            }
        }
        let mut out = vec![];
        for p in parents {
            let mut rekeyed = None;
            for (_, id, h, order) in changed.iter().filter(|c| c.0 == p) {
                if st.reader().get_block(h)?.order != *order {
                    rekeyed = Some(id.clone());
                    break;
                }
            }
            if let Some(sib) = rekeyed {
                let at = p
                    .as_ref()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "the top level".into());
                out.push(Issue {
                    severity: Severity::Warning,
                    code: "W_SIBLINGS_REKEYED",
                    block: p.clone(),
                    message: format!(
                        "the children of {at} were re-keyed in canon since this view was based on it (e.g. {sib}): check the order of the blocks this proposal places there"
                    ),
                    fix: format!(
                        "after landing, read the children of {at} and move any block this proposal placed out of order"
                    ),
                });
            }
        }
        Ok(out)
    }

    /// K1: the change that gives each block in `re` its re-derived key in view `view`, from the
    /// view's current version of the block, with the deps the store will give it. Every dep must
    /// be among `ids`, the changes proposed: a subset that leaves out a later edit of the block
    /// cannot carry the re-key.
    fn rekey_change(
        &self,
        st: &ReadState,
        s: &Session,
        view: &str,
        re: &[(BlockId, String)],
        ids: &[Hash],
    ) -> Result<Change, RdyError> {
        let vm = st.materialized(&Scope::View(view.to_string()))?;
        let mut ops = vec![];
        for (id, k) in re {
            let cur = vm.get(id).ok_or_else(|| {
                RdyError::new(
                    E_UNKNOWN_BLOCK,
                    format!("{id} is proposed from view {view:?} but the view no longer has it"),
                    "propose the whole view",
                )
            })?;
            let mut b = cur.clone();
            b.fields.insert("key".into(), FieldValue::Str(k.clone()));
            ops.push(Op::Update {
                base: cur.version_hash(),
                block: b,
            });
        }
        let names = re
            .iter()
            .map(|(id, k)| format!("{id} {k}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut change = Change {
            view: view.to_string(),
            ops,
            deps: vec![],
            provenance: s.provenance(),
            justification: None,
            message: format!("new keys rdy derived, as another block holds the old ones: {names}"),
            links: ChangeLinks::default(),
            read_identity: None,
        };
        change.deps = self.store.change_deps(&change, Some(view))?;
        if let Some(d) = change.deps.iter().find(|d| !ids.contains(d)) {
            return Err(RdyError::new(
                E_DEPS_MISSING,
                format!(
                    "rdy must give {names} a new key (another block holds the old one), and that builds on change {}, which this proposal leaves out",
                    d.short()
                ),
                format!(
                    "add {} to the proposal, or propose the whole view",
                    d.short()
                ),
            ));
        }
        Ok(change)
    }

    /// Writes the key change (message `message`, from `rekey_change`) that gives each block in
    /// `re` its re-derived key, as `Rdy::assign_keys` writes its keys: it briefs exactly those
    /// blocks, cites that briefing, and marks the keys `key_auto`. The change commits and
    /// publishes in its own batch (D-ME2HD, O1, O2).
    fn write_rekey(
        &mut self,
        s: &Session,
        view: &str,
        message: String,
        re: &[(BlockId, String)],
        alternative: Option<AltChoice>,
    ) -> Result<WriteResult, RdyError> {
        let brief = self.read().brief(
            &Scope::View(view.to_string()),
            &Coverage {
                ids: re.iter().map(|(id, _)| id.to_string()).collect(),
                path: None,
            },
            None,
        )?;
        brief.register(&self.briefs);
        let token = brief.base.token.clone();
        let ops: Vec<OpInput> = re
            .iter()
            .map(|(id, k)| OpInput::Update {
                id: id.to_string(),
                base: None,
                fields: BTreeMap::from([("key".to_string(), serde_json::Value::String(k.clone()))]),
                unset: vec![],
                body: None,
                links: None,
                block_type: None,
                move_to: None,
            })
            .collect();
        let ours: BTreeSet<BlockId> = re.iter().map(|(id, _)| id.clone()).collect();
        self.write_marked(
            s,
            WriteRequest {
                view: view.to_string(),
                ops,
                justification: None,
                message,
            },
            ChangeLinks {
                alternative,
                ..ChangeLinks::default()
            },
            Some(&token),
            &ours,
            None,
        )
    }

    /// Proposes view `view`'s included changes (a new version of its open proposal, if any),
    /// with `summary` as `propose_subset` stores it.
    pub fn propose(
        &mut self,
        s: &Session,
        view: &str,
        title: &str,
        summary: Option<&str>,
    ) -> Result<ProposalInfo, RdyError> {
        self.propose_with(s, view, title, None, summary, None)
    }

    /// Proposes `changes` of view `view` (its whole included set when `None`): a new version of
    /// the view's open proposal, or a new proposal. A subset must be closed under dependencies
    /// (each dep in the subset or landed in the view's base). Never refuses for a conflict:
    /// `conflicts` lists every conflict of the view and `blocking` the ones on blocks the version
    /// writes, which refuse its landing until they are resolved. A non-blank `summary` is stored
    /// with the version; a version proposed without one keeps the last version's.
    pub fn propose_subset(
        &mut self,
        s: &Session,
        view: &str,
        title: &str,
        changes: Option<&[String]>,
        summary: Option<&str>,
    ) -> Result<ProposalInfo, RdyError> {
        self.propose_with(s, view, title, changes, summary, None)
    }

    /// `propose_subset` with the alternative the version chooses (altg1). A member view of an
    /// open set must name its own member (`E_ALT_UNCHOSEN`); a pick must name an open set the
    /// view is a member of, and the view's own member by label or view name (`E_ALT_MEMBER`).
    /// A view a chosen or abandoned set did not choose proposes nothing, and a change written in
    /// a member view of an open set is proposed only from that view, choosing it (`E_ALT_MEMBER`).
    pub fn propose_with(
        &mut self,
        s: &Session,
        view: &str,
        title: &str,
        changes: Option<&[String]>,
        summary: Option<&str>,
        alternative: Option<&AltPick>,
    ) -> Result<ProposalInfo, RdyError> {
        self.propose_plan(
            s,
            ProposePlanRequest {
                view,
                title,
                changes,
                summary,
                alternatives: alternative,
                plan_answers: &[],
            },
        )
    }

    /// Proposes `req` as `propose_with` does, and records its plan answers in the same batch
    /// (D-4JMPH): every propose writes one plan version bound to the version it writes, its
    /// answers validated against the version's candidate before anything is appended (a refused
    /// answer writes nothing). The reply's `plan` summarizes that plan.
    pub fn propose_plan(
        &mut self,
        s: &Session,
        req: ProposePlanRequest<'_>,
    ) -> Result<ProposalInfo, RdyError> {
        self.propose_relayed(s, req, None)
    }

    /// `propose_with` on the person's word (O32, C3, D-KQ4CA), under the relay table
    /// (`answer::relay::relayed`, D-HA7YZ). `person` is the project's human actor, from the
    /// caller (O18); `implicit` is true over MCP, where the session is the relay. The cases,
    /// closed:
    /// - a human session with no relay: `propose_with` as the session;
    /// - a relay of a view the person owns, or of the person's default `work`: `propose_with` as
    ///   the person (the version's `provenance`), and an `ANSWER` (act `propose`, item
    ///   `propose:<view>`, `targets` the version, evidence `text`) in the version's batch (O14);
    /// - a relay of any other view: today's ownership rule for the session (`check_view_op`),
    ///   and a view the session may propose itself is refused (a relay proposes the person's
    ///   view only);
    /// - any other case is the table's refusal, in the answer's words (O19).
    #[allow(clippy::too_many_arguments)]
    pub fn propose_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        implicit: bool,
        view: &str,
        title: &str,
        changes: Option<&[String]>,
        summary: Option<&str>,
        alternative: Option<&AltPick>,
    ) -> Result<ProposalInfo, RdyError> {
        self.propose_plan_as(
            s,
            person,
            relay,
            implicit,
            ProposePlanRequest {
                view,
                title,
                changes,
                summary,
                alternatives: alternative,
                plan_answers: &[],
            },
        )
    }

    /// `propose_as` with `req`'s plan answers recorded in the same batch (`propose_plan`): the
    /// answers are the person's when the relay proposes as the person.
    pub fn propose_plan_as(
        &mut self,
        s: &Session,
        person: &str,
        relay: &RelayInput,
        mcp: bool,
        req: ProposePlanRequest<'_>,
    ) -> Result<ProposalInfo, RdyError> {
        let (who, relay) = relayed(s, person, relay, mcp).map_err(|e| e.error())?;
        let Some(relay) = relay else {
            return self.propose_relayed(s, req, None);
        };
        let view = req.view;
        let st = self.read();
        let persons = view == "work"
            || st
                .view_head(view)
                .and_then(|h| h.owner.as_ref())
                .is_some_and(|o| o.actor == person);
        if !persons {
            crate::views::check_view_op(&st, s, view, Action::Propose)?;
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("view {view:?} is not the person's: a relay proposes only their view"),
                "drop the relay to propose your own view",
            ));
        }
        drop(st);
        self.propose_relayed(&who, req, Some(relay))
    }

    /// The one propose writer (`propose_plan`), as `s`, with the person's relay when `s` is the
    /// person (`propose_plan_as`): every check, the version's candidate, its plan and its
    /// answers are checked first; then the key change rdy adds (if any) is written in its own
    /// batch, and the plan computed again after it (O1); then readiness runs on the version,
    /// and the version, the relay's answer, the bound plan and, for a ready version, its
    /// assessment are written in one batch, and published once (D-ME2HD).
    fn propose_relayed(
        &mut self,
        s: &Session,
        req: ProposePlanRequest<'_>,
        relay: Option<Relay>,
    ) -> Result<ProposalInfo, RdyError> {
        let ProposePlanRequest {
            view,
            title,
            changes,
            summary,
            alternatives: alternative,
            plan_answers,
        } = req;
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot propose", s.role),
                "start the session as a writer",
            ));
        }
        let st = self.read();
        crate::views::check_view_op(&st, s, view, Action::Propose)?;
        // D-K0SS1 (R2): a closed view takes no proposal
        if st.view_head(view).is_some_and(|h| h.closed) {
            return Err(rdy_store::StoreError::ViewClosed(view.to_string()).into());
        }
        let alternative = proposed_alt(&st, view, alternative)?;
        let included = st.included(view);
        if included.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("view {view:?} has no changes to propose"),
                "write to the view first",
            ));
        }
        let subset = changes.is_some();
        let mut ids: Vec<Hash> = match changes {
            None => included.as_ref().clone(),
            Some(list) => {
                let in_view: BTreeSet<&Hash> = included.iter().collect();
                let mut seen = BTreeSet::new();
                let mut out = vec![];
                for text in list {
                    let h = parse_change(text)?;
                    if !in_view.contains(&h) {
                        return Err(RdyError::new(
                            E_BAD_INPUT,
                            format!("change {} is not in view {view:?}", h.short()),
                            format!("rdy view show {view} lists its changes"),
                        ));
                    }
                    if seen.insert(h) {
                        out.push(h);
                    }
                }
                if out.is_empty() {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        "name at least one change to propose",
                        format!(
                            "rdy view show {view} lists its changes; omit changes to propose the whole view"
                        ),
                    ));
                }
                out
            }
        };
        let changes = changes_of(&st, &ids)?;
        linked_alts(&st, view, alternative.as_ref(), &ids, &changes)?;
        // closure: each dep is proposed with its dependent or landed in the view's base (the
        // landings at or before the base are collected once, when a dep first needs them)
        let base = st.effective_base(view);
        let set: BTreeSet<&Hash> = ids.iter().collect();
        let mut based: Option<BTreeSet<Hash>> = None;
        // (the change, what it needs, why)
        let mut missing: Vec<(Hash, Hash, String)> = vec![];
        for (c, id) in changes.iter().zip(&ids) {
            for dep in &c.deps {
                if set.contains(dep) {
                    continue;
                }
                let in_base = match st.reader().landing_of(dep)? {
                    Some(l) => {
                        if based.is_none() {
                            based = Some(landings_through(&st, base)?);
                        }
                        based.as_ref().is_some_and(|b| b.contains(&l))
                    }
                    None => false,
                };
                if !in_base {
                    missing.push((
                        *id,
                        *dep,
                        format!("{} depends on {}", id.short(), dep.short()),
                    ));
                }
            }
        }
        // the parent rule (final review fix round 1): a subset also carries the change that
        // creates each parent its blocks are created or moved under, unless the parent is in the
        // view's base (the whole view carries every such change it has)
        if subset {
            let sub: Vec<(Hash, Change)> =
                ids.iter().copied().zip(changes.iter().cloned()).collect();
            let mut in_base = BaseBlocks::new(&st, base);
            let parents = missing_parents(&sub, &mut |p| in_base.has(p))?;
            if !parents.is_empty() {
                let rest: Vec<(Hash, Change)> = included
                    .iter()
                    .filter(|h| !set.contains(h))
                    .map(|h| Ok((*h, st.reader().change(h)?)))
                    .collect::<Result<_, RdyError>>()?;
                let by = creators(rest.iter().map(|(h, c)| (*h, c)));
                for (c, block, p) in parents {
                    if let Some(k) = by.get(&p)
                        && !missing.iter().any(|(x, d, _)| *x == c && d == k)
                    {
                        missing.push((
                            c,
                            *k,
                            format!(
                                "{} puts {block} under {p}, which {} creates",
                                c.short(),
                                k.short()
                            ),
                        ));
                    }
                }
            }
        }
        if !missing.is_empty() {
            let mut deps: Vec<Hash> = vec![];
            for (_, d, _) in &missing {
                if !deps.contains(d) {
                    deps.push(*d);
                }
            }
            let shorts = deps.iter().map(Hash::short).collect::<Vec<_>>();
            let mut e = RdyError::new(
                E_DEPS_MISSING,
                format!(
                    "the proposal is not closed under dependencies: {} missing",
                    deps.len()
                ),
                format!(
                    "add {} to the proposal, or propose the whole view",
                    shorts.join(", ")
                ),
            );
            e.issues = missing
                .into_iter()
                .map(|(_, d, message)| Issue {
                    severity: Severity::Error,
                    code: E_DEPS_MISSING,
                    block: None,
                    message,
                    fix: format!("include {}", d.short()),
                })
                .collect();
            return Err(e);
        }
        let conflicts = conflict_infos(&st, &st.conflicts(&Scope::View(view.to_string()))?)?;
        let blocking_raw = blocking_conflicts(&st, view, &ids)?;
        // tgat1: a tension on blocks that all carry a blocking conflict is reported as the conflict
        let conflicted: BTreeSet<BlockId> = blocking_raw.iter().map(|c| c.block.clone()).collect();
        let blocking = conflict_infos(&st, &blocking_raw)?;
        // a version with a blocking conflict cannot land until it is resolved, and its merge onto
        // canon would refuse at the first conflict: the gate validates it at landing (RF10)
        let mut issues = vec![];
        let mut coverage_error: Option<String> = None;
        let mut rekey = None;
        if blocking.is_empty() {
            // K1 (FW-PR3): a key rdy derived for a block this version creates, held by another
            // block in the prospective canon, is re-derived by a change rdy adds to the view
            let (next, applied) = prospective(&st, &changes, None)?;
            let vm = st.materialized(&Scope::View(view.to_string()))?;
            let re = crate::keys::rederive(
                &next,
                &rekeyable_in(&changes, &applied, &*st.materialized(&Scope::Canon)?),
                &vm,
            );
            if re.is_empty() {
                issues = prospective_issues_of(&st, &changes)?;
            } else {
                let mut change = self.rekey_change(&st, s, view, &re, &ids)?;
                // alts1: a change in a member view of an open set records the set, as a write does
                change.links.alternative = alternative.clone();
                let mut all = changes.clone();
                all.push(change.clone());
                issues = self.prospective_issues(&st, &all, None)?.0;
                rekey = Some((change, re));
            }
            if let Some(first) = issues.iter().find(|i| i.severity == Severity::Error) {
                let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
                e.issues = issues;
                return Err(e);
            }
            // a view with no head floats on canon
            let view_base = st.view_head(view).map_or(st.canon_head(), |h| h.base);
            issues.extend(self.rekeyed_warnings(&st, view_base, &changes)?);
            // cc16 (FW-CH5): a decision this proposal lands with no content that applies it.
            // It never refuses; a read error leaves the warnings out and is reported.
            match unapplied_warnings(&st, view, &changes) {
                Ok(w) => issues.extend(w),
                Err(e) => coverage_error = Some(crate::coverage::coverage_error_text(&e)),
            }
        }
        // the candidate: the version's changes with the key change rdy adds, which are what it
        // lands (D-4JMPH). A blocking conflict leaves it without one until it is resolved.
        let candidate: Option<Vec<Change>> = blocking.is_empty().then(|| {
            let mut all = changes.clone();
            all.extend(rekey.as_ref().map(|(c, _)| c.clone()));
            all
        });
        if candidate.is_none() && !plan_answers.is_empty() {
            let ids: Vec<String> = blocking.iter().map(|c| c.block.to_string()).collect();
            return Err(RdyError::new(
                E_CONFLICT_UNRESOLVED,
                crate::consequence::conflict_message(&ids),
                crate::consequence::CONFLICT_FIX,
            ));
        }
        // pver1: a view with an open proposal (its latest version has not landed) gets the next
        // version of it; otherwise a new proposal starts
        let mut open = None;
        if let Some(b) = st.reader().get_ref(&format!("open/{view}"))?
            && b.len() == 32
        {
            let stable = ref_hash(&b, "an open ref")?;
            let latest = latest_version(&st, &stable)?;
            if landed_as(&st, &st.reader().proposal(&latest)?)?.is_none() {
                open = Some((stable, latest));
            }
        }
        // the version's provenance, its plan's and its new answers' (one clock read)
        let by = s.provenance();
        // ruling task-5 F3 (Amendment 1): the plan answers are checked first, against the plan
        // before rdy's key change, writing nothing
        let mut plan = crate::plan::lifecycle::prepare_proposal_plan(
            &st,
            &by,
            view,
            open.as_ref().map(|(_, latest)| latest),
            candidate.as_deref(),
            plan_answers,
        )?;
        // D-ME2HD (O1, O2): the key change rdy adds is written now, after every check above and
        // before the version, in its own batch: readiness reads the tension sets of published
        // changes, so the change is in the view before the version is read, and it records the
        // briefing of the blocks it re-keys. The version lists it last, and only it besides the
        // changes proposed. The plan the version binds is computed again after it, so its
        // tension ids are the ones every later read of the version computes, with each answer to
        // a tension the key change re-keyed carried to it (rulings task-5 F3, task-15 F2).
        let mut keys = BTreeMap::new();
        if let Some((change, re)) = rekey.take() {
            keys = re
                .iter()
                .map(|(id, k)| (id.to_string(), k.clone()))
                .collect();
            let written = self.write_rekey(s, view, change.message, &re, alternative.clone())?;
            let key_change = parse_change(&written.change)?;
            ids.push(key_change);
            let after = self.read();
            let mut keyed = changes.clone();
            keyed.push(after.reader().change(&key_change)?);
            let before = crate::plan::lifecycle::BeforeKeyChange {
                st: &st,
                changes: candidate.as_deref().unwrap_or_default(),
            };
            plan = crate::plan::lifecycle::prepare_proposal_plan_keyed(
                &after,
                Some(before),
                &by,
                view,
                open.as_ref().map(|(_, latest)| latest),
                Some(&keyed),
                plan_answers,
            )?;
        }
        issues.extend(plan.warnings.iter().cloned());
        let count = ids.len();
        let chosen = alternative.as_ref().map(|a| AltRef {
            set: alt_label(&a.set),
            member: a.member.clone(),
        });
        // the relay's ANSWER: the person proposes, the agent relays (O32); the store sets targets
        let answer = relay.as_ref().map(|r| Answer {
            item: format!("propose:{view}"),
            act: AnswerAct::Propose,
            option: None,
            label: None,
            options: vec![],
            card_hash: None,
            targets: vec![],
            person: by.clone(),
            relayed_by: Some(r.by.clone()),
            relay_evidence: Some(r.evidence.clone()),
            evidence_kind: Some(EvidenceKind::Text),
            note: None,
        });
        let answered = answer.is_some();
        let n = match &open {
            Some((_, latest)) => version_number(&st, latest)? + 1,
            None => 1,
        };
        let to_rdy = |e: rdy_store::StoreError| {
            if answered {
                crate::version::answer_write_error(e)
            } else {
                e.into()
            }
        };
        let mut prepared = self
            .store
            .prepare_propose(rdy_store::plan::ProposeRequest {
                view,
                title,
                summary,
                by: by.clone(),
                changes: ids,
                alternative,
                prev: open.map(|(_, latest)| latest),
                rekey: None,
                answer,
            })
            .map_err(to_rdy)?;
        let id = prepared.version();
        let stable = prepared.stable();
        let pref = ProposalRef {
            version: id,
            stable,
            n,
        };
        let bound = plan.plan(view, id, by);
        let plan_hash = self.store.bind_plan(&mut prepared, &bound)?;
        // D-ME2HD, D-3JZP4: readiness on the version this batch writes, with the plan it binds;
        // a ready version's assessment joins the batch (D-SDSV2). One that is not ready is
        // stored as it is, with no assessment.
        let assessed = {
            let now = self.read();
            let c = crate::readiness::Candidate {
                pref,
                p: prepared.proposal().clone(),
                plan: crate::readiness::PlanSource::InHand {
                    plan: bound,
                    hash: plan_hash,
                    computed: plan.computed.take(),
                },
            };
            // a read error never fails the reply (as `tensions_error`): the version is stored,
            // not ready, with the error as its one issue
            crate::readiness::assess(&now, &c, &now.gate_config()).unwrap_or_else(|e| {
                crate::readiness::Assessed {
                    readiness: crate::readiness::Readiness {
                        issues: vec![Issue {
                            severity: Severity::Error,
                            code: e.code,
                            block: None,
                            message: e.message,
                            fix: e.fix,
                        }],
                        summary: PlanSummary::default(),
                        card: None,
                        policy: None,
                    },
                    assessment: None,
                }
            })
        };
        let assessment = match &assessed.assessment {
            Some(a) => Some(self.store.bind_assessment(&mut prepared, a)?),
            None => None,
        };
        let written = self.store.commit_propose(prepared)?;
        let answer = written.answer;
        self.publish(Delta::Seq)?;
        let stored = self.store.proposal(&id)?;
        let canon_at = stored.canon_at;
        // tgat1: the version's tensions, read after a re-key change (if any) was committed. The
        // proposal is stored and published by now: a read error leaves the tensions out and is
        // reported, as a briefing's `tensions_error` is, and the reply keeps the proposal.
        let (tensions, tensions_omitted, mut tensions_error) = match self.read().proposal_tensions(
            view,
            &stored.changes,
            &conflicted,
            PROPOSAL_TENSIONS_MAX,
        ) {
            Ok((t, n)) => (t, n, None),
            Err(e) => {
                let text = crate::coverage::coverage_error_text(&e);
                // coverage reads the same tension set: it is not worked out either
                coverage_error.get_or_insert_with(|| text.clone());
                (vec![], 0, Some(text))
            }
        };
        // D-WMCFW: the digest of every advised tension, listed or not, and the advised ones the
        // list leaves out; a read error is the tensions' (the same set)
        let (advised_digest, advised_omitted) = match self.read().advised_of(view, &stored.changes)
        {
            Ok(advised) => {
                let listed: BTreeSet<&str> = tensions.iter().map(|t| t.tension.as_str()).collect();
                let omitted = advised
                    .iter()
                    .filter(|a| {
                        !a.open.record.blocking
                            && !listed.contains(a.tension.id.to_string().as_str())
                    })
                    .count();
                (
                    crate::semantic::accept_advice::advised_digest(&advised),
                    omitted,
                )
            }
            Err(e) => {
                tensions_error.get_or_insert_with(|| crate::coverage::coverage_error_text(&e));
                (None, 0)
            }
        };
        Ok(ProposalInfo {
            id: id.to_string(),
            stable: stable.to_string(),
            version: n,
            r#ref: pref.to_string(),
            title: title.into(),
            summary: stored.summary,
            changes: count,
            warnings: issues,
            conflicts,
            blocking,
            tensions,
            tensions_omitted,
            advised_digest,
            advised_omitted,
            canon_at: canon_at.map(|h| h.to_string()),
            alternative: chosen,
            keys,
            tensions_error,
            coverage_error,
            relayed_by: relay.map(|r| r.by),
            answer: answer.map(|a| a.to_string()),
            plan: plan.summary,
            ready: assessed.assessment.is_some(),
            issues: assessed.readiness.issues,
            assessment: assessment.map(|h| h.to_string()),
            policy: assessed
                .assessment
                .as_ref()
                .and_then(|a| a.policy.as_ref())
                .map(crate::readiness::ProposalPolicy::from),
        })
    }

    /// Resolves a proposal reference: a version hash, `P-<prefix>` (the proposal's latest
    /// version) or `P-<prefix>@<n>` (its version `n`, counted from 1).
    pub fn resolve_proposal(&self, text: &str) -> Result<ProposalRef, RdyError> {
        resolve_in(&self.read(), text)
    }

    /// The landings of canon's chain since `base`, head first: every landing when `base` is none
    /// or not on the chain.
    fn landings_since(
        st: &ReadState,
        base: Option<Hash>,
    ) -> Result<Vec<(Hash, LandingRecord)>, RdyError> {
        let mut since = vec![];
        let mut cur = st.canon_head();
        while let Some(h) = cur {
            if Some(h) == base {
                break;
            }
            let rec = st.reader().landing(&h)?;
            cur = rec.prev;
            since.push((h, rec));
        }
        Ok(since)
    }

    /// FW-HB1 and RULING F1's limit (I7): `strict` or `reviews.require_up_to_date` refuse any
    /// landing since version `p` was proposed (`p.canon_at`), with today's texts and no fresh
    /// card.
    fn check_base_moved(
        st: &ReadState,
        pref: &ProposalRef,
        p: &Proposal,
        opts: &LandOptions,
    ) -> Result<(), RdyError> {
        if !opts.strict && !st.require_up_to_date() {
            return Ok(());
        }
        let intervening = Self::landings_since(st, p.canon_at)?;
        if intervening.is_empty() {
            return Ok(());
        }
        let since = format!(
            "proposal {pref} moved: canon has {} landing(s) since it was proposed ({})",
            intervening.len(),
            intervening
                .iter()
                .map(|(h, _)| h.short())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if opts.strict {
            return Err(RdyError::new(
                E_PROPOSAL_MOVED,
                since,
                "approve without --strict to accept a clean rebase, or ask its author to propose again on the current canon",
            ));
        }
        Err(RdyError::new(
            E_PROPOSAL_MOVED,
            since,
            format!(
                "this project requires proposals on the current canon (reviews.require_up_to_date): its author must propose again (rdy propose --view {}); then approve the new version",
                p.view
            ),
        ))
    }

    /// D-WQVGT, D-ME2HD "Freshness-only landing": the stored assessment of version `pref`
    /// (record `p`) still holds. It reads the assessment and the landings since its base, and
    /// computes nothing for the approver. A version with no stored assessment is not ready: its
    /// readiness issues, or the O10 issue when it has none. A landing since the assessment's base
    /// that wrote a block of its `reach`, then a pack, then a gate config other than the
    /// assessment's, refuses with `E_PROPOSAL_MOVED` and a fresh card ([`Rdy::moved_fresh`],
    /// RULING F1). M3c O16 adds, for every lander: (1) a live policy other than the assessment's;
    /// (2) under `coverage.gate: no_lower`, a landing since the base that wrote a block of one of
    /// the assessment's pages; and for [`Lander::Policy`] only: (3) under an accepting rule with
    /// `lifecycle: draft`, a written block in stable content in canon now. Each refuses the same
    /// way. Returns the assessment's hash.
    fn fresh_assessment(
        &mut self,
        st: &ReadState,
        pref: &ProposalRef,
        p: &Proposal,
        lander: Lander,
    ) -> Result<Hash, RdyError> {
        let Some(h) = st
            .reader()
            .assessment_of_proposal(&pref.version, st.seq())?
        else {
            // O10: the one readiness, on the plan the store binds to the version
            let c = crate::readiness::Candidate::stored(st, *pref, p.clone())?;
            let mut issues = crate::readiness::readiness(st, &c)?.issues;
            if issues.is_empty() {
                issues.push(no_stored_assessment(pref));
            }
            return Err(proposal_not_ready(pref, issues));
        };
        let a = st.reader().assessment(&h)?;
        let reach: BTreeSet<&BlockId> = a.reach.iter().collect();
        let since = Self::landings_since(st, a.base)?;
        for (l, rec) in since.iter().rev() {
            let mut hit = BTreeSet::new();
            for c in &rec.changes {
                for op in &st.reader().change(c)?.ops {
                    if reach.contains(op.block_id()) {
                        hit.insert(op.block_id().to_string());
                    }
                }
            }
            if !hit.is_empty() {
                let ids: Vec<String> = hit.into_iter().collect();
                let message = format!(
                    "proposal {pref} moved: landing {} changed {} after its assessment",
                    l.short(),
                    ids.join(", ")
                );
                return Err(self.moved_fresh(st, pref, p, message)?);
            }
        }
        if pack_digest(st.pack()) != a.pack.digest {
            let message = format!("proposal {pref} moved: the pack changed after its assessment");
            return Err(self.moved_fresh(st, pref, p, message)?);
        }
        if config_digest(&st.gate_config()) != a.config {
            let message =
                format!("proposal {pref} moved: the gate config changed after its assessment");
            return Err(self.moved_fresh(st, pref, p, message)?);
        }
        if let Some(message) = policy_moved(st, pref, p, &a, &since, lander)? {
            return Err(self.moved_fresh(st, pref, p, message)?);
        }
        Ok(h)
    }

    /// RULING F1: the refusal of a failed freshness check of version `pref` (record `p`), with
    /// `message`. rdy computes the version's readiness again on `st`. A ready version gets a
    /// fresh `ASSESSMENT` on canon's head, written in a batch of its own and published, and the
    /// refusal carries its card in `corrected` (`{"card": ...}`, the shape [`card_of`] gives);
    /// the next approval binds that assessment. A version that is not ready now gets its fresh
    /// readiness issues and no card (O11).
    fn moved_fresh(
        &mut self,
        st: &ReadState,
        pref: &ProposalRef,
        p: &Proposal,
        message: String,
    ) -> Result<RdyError, RdyError> {
        let c = crate::readiness::Candidate::stored(st, *pref, p.clone())?;
        let assessed = crate::readiness::assess(st, &c, &st.gate_config())?;
        let (Some(mut a), Some(mut card)) = (assessed.assessment, assessed.readiness.card) else {
            let mut e = RdyError::new(E_PROPOSAL_MOVED, message, O11_FIX);
            e.issues = assessed.readiness.issues;
            return Ok(e);
        };
        // the fresh record assesses the version on the canon it was computed on, so the next
        // approval's freshness reads the landings after it
        a.base = st.canon_head();
        card.card = crate::pending::render_checked(&card, crate::pending::CARD_DEFAULT_TOKENS)?;
        card.card_hash = crate::pending::card_hash(&card).to_string();
        self.store
            .put_assessment(&a)
            .map_err(crate::answer::internal)?;
        self.publish(Delta::Seq)?;
        Ok(RdyError::new(
            E_PROPOSAL_MOVED,
            message,
            format!("review the fresh card in corrected and approve {pref} again"),
        )
        .with_corrected(serde_json::json!({ "card": card_of(&card) })))
    }

    /// The freshness of the stored assessment of the proposal version `proposal` names
    /// ([`Rdy::fresh_assessment`]), before an approval of a card whose hash is not the one shown
    /// (`answer::bind`). A failed check writes what its refusal says (a fresh assessment, RULING
    /// F1) and nothing else. A version that is not the latest, or whose base moved under
    /// `reviews.require_up_to_date`, gets no fresh card (F1's limit): Ok, and the caller's own
    /// refusal stands.
    pub(crate) fn check_fresh(&mut self, proposal: &str) -> Result<(), RdyError> {
        let st = self.read();
        let pref = resolve_in(&st, proposal)?;
        let p = st.reader().proposal(&pref.version)?;
        if latest_version(&st, &pref.stable)? != pref.version
            || Self::check_base_moved(&st, &pref, &p, &LandOptions::default()).is_err()
        {
            return Ok(());
        }
        self.fresh_assessment(&st, &pref, &p, Lander::Approver)
            .map(|_| ())
    }

    /// Whether the proposal version `proposal` names carries a content removal request. Any
    /// read error is false: the caller's own refusal stands.
    fn carries_removal(&self, proposal: &str) -> bool {
        let st = self.read();
        resolve_in(&st, proposal)
            .and_then(|pref| Ok(st.reader().proposal(&pref.version)?))
            .and_then(|p| crate::redaction::requests_carried(&st, &p.changes))
            .is_ok_and(|r| !r.is_empty())
    }

    /// Lands a proposal; returns the landing id.
    pub fn land(&mut self, s: &Session, proposal: &str) -> Result<String, RdyError> {
        self.land_report(s, proposal).map(|l| l.landing)
    }

    /// Lands a proposal; returns the landing id and the gate's warnings.
    pub fn land_report(&mut self, s: &Session, proposal: &str) -> Result<LandInfo, RdyError> {
        self.land_with(s, proposal, LandOptions::default())
    }

    /// Lands the proposal version `proposal` names (`P-x`, `P-x@n` or a version hash); returns
    /// the landing, what was approved, the gate's warnings and the views the landing rebased.
    pub fn land_with(
        &mut self,
        s: &Session,
        proposal: &str,
        opts: LandOptions,
    ) -> Result<LandInfo, RdyError> {
        let prepared = self.prepare_landing(s, proposal, opts)?;
        self.commit_landing(prepared, &[])
    }

    /// `rdy_land` (D-ZWP7Q "The agent's landing act", O14): an agent session of role writer or
    /// reviewer lands the version `proposal` names when its stored verdict accepts it and the
    /// session's actor matches the accepting rule's `actors` (O6). Every check of an approval
    /// runs in its order, with the policy's acceptance in place of the approver's role. The
    /// landing records `assurance: policy`, the live policy version and the assessment, with no
    /// relay ([`rdy_store::Store::prepare_land_by_policy`]); it is never signed (D-DKAMT).
    pub fn land_by_policy(&mut self, s: &Session, proposal: &str) -> Result<LandInfo, RdyError> {
        let prepared =
            self.prepare_landing_as(s, proposal, LandOptions::default(), Lander::Policy)?;
        self.commit_landing(prepared, &[])
    }

    /// Every check of `land_with` and the complete landing it would write, under the writer and
    /// without writing anything: the acceptance work, the frames and the landing record, with
    /// every clock value it uses already read. [`Rdy::commit_landing`] writes it. One refusal
    /// writes: a failed freshness check of a version that is ready now writes its fresh
    /// assessment, in a batch of its own (RULING F1, [`Rdy::moved_fresh`]).
    pub(crate) fn prepare_landing(
        &mut self,
        s: &Session,
        proposal: &str,
        opts: LandOptions,
    ) -> Result<PreparedApproval, RdyError> {
        self.prepare_landing_as(s, proposal, opts, Lander::Approver)
    }

    /// [`Rdy::prepare_landing`] by `lander`. [`Lander::Policy`] takes O14's session checks in
    /// place of the approver's role, adds O16's draft check to freshness, and after the holds
    /// reads the stored verdict (O14) and the lander's actor (O6).
    pub(crate) fn prepare_landing_as(
        &mut self,
        s: &Session,
        proposal: &str,
        opts: LandOptions,
        lander: Lander,
    ) -> Result<PreparedApproval, RdyError> {
        match lander {
            Lander::Approver if !s.role.can_land() => {
                // D-CC90E: an agent that asks to land a content removal is told who must approve
                // it
                if !s.is_human() && self.carries_removal(proposal) {
                    return Err(rdy_store::StoreError::RedactionHuman.into());
                }
                return Err(RdyError::new(
                    E_PERMISSION,
                    format!("role {:?} cannot land proposals", s.role),
                    "ask an approver to land it",
                ));
            }
            Lander::Approver => {}
            // O14: the policy's acceptance replaces the approval, for an agent session of role
            // writer or reviewer only
            Lander::Policy if s.is_human() => {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "rdy land is for agent sessions; a person approves",
                    "approve with rdy approve",
                ));
            }
            Lander::Policy if !s.role.can_write() => {
                return Err(RdyError::new(
                    E_PERMISSION,
                    format!("role {:?} cannot land proposals", s.role),
                    "start the session as a writer",
                ));
            }
            Lander::Policy => {}
        }
        let st = self.read();
        let pref = resolve_in(&st, proposal)?;
        let evidence = relay_evidence_kind(s, proposal, &pref, &opts)?;
        let pid = pref.version;
        let p = st.reader().proposal(&pid)?;
        // D-CC90E: content removal lands only with a person's approval, local or relayed; an
        // agent session never approves it, whatever role it claims
        let requests = crate::redaction::requests_carried(&st, &p.changes)?;
        if !requests.is_empty() && !s.is_human() {
            return Err(rdy_store::StoreError::RedactionHuman.into());
        }
        // land1: a landed change is already in canon; replaying it would only fail obscurely
        if let Some(landing) = landed_as(&st, &p)? {
            return Err(RdyError::new(
                E_ALREADY_LANDED,
                format!(
                    "proposal {} already landed as landing {landing}",
                    pid.short()
                ),
                format!(
                    "nothing to do: see it in rdy log; to land later work from view {:?}, propose the view again",
                    p.view
                ),
            ));
        }
        // pver1: the approval is bound to the version named; a newer version moves it
        let latest = latest_version(&st, &pref.stable)?;
        if latest != pid {
            return Err(moved(&st, &pref, &latest, &p.view)?);
        }
        // D-K0SS1: a close of the version's view withdrew it; a losing member's version meets
        // this before the alternative checks (R1)
        if withdrawn(&st, &pid, &p)? {
            return Err(withdrawn_error(&pref, &p.view));
        }
        // altg1: the set is re-checked at approval, before the base check, so a set an earlier
        // landing chose is reported as such
        let alt = landing_alt(&st, &p)?;
        let chosen = alt.as_ref().map(|a| AltRef {
            set: alt_label(&a.set),
            member: a.member.clone(),
        });
        let changes = changes_of(&st, &p.changes)?;
        linked_alts(&st, &p.view, p.alternative.as_ref(), &p.changes, &changes)?;
        // FW-HB1, RULING F1's limit: --strict and reviews.require_up_to_date refuse any landing
        // since the version was proposed
        Self::check_base_moved(&st, &pref, &p, &opts)?;
        // D-WQVGT, D-ME2HD: freshness only, against the version's stored assessment: its reach,
        // the pack and the gate config (a failed check gives a fresh card, RULING F1)
        let assessment = self.fresh_assessment(&st, &pref, &p, lander)?;
        // D-ME2HD "Later refusals", built with no candidate (I8): a rejected decision or one_way
        // re-affirmation, refused deferrals, an advice accepted after the assessment
        let (decisions, reaffirms) = own_decisions(&st, &changes)?;
        let bound = st
            .reader()
            .plan_of_proposal(&pref.version, st.seq())?
            .is_some();
        let mut later =
            crate::readiness::later_refusals(&st, &pref, &p.view, &decisions, &reaffirms, bound)?;
        // ruling task-12 R1 (D-WMCFW): an advice accepted after the assessment blocks
        let record = st.reader().assessment(&assessment)?;
        later.extend(crate::readiness::accepted_advice(&st, &pref, &p, &record)?);
        if !later.is_empty() {
            return Err(proposal_not_ready(&pref, later));
        }
        // the ops fold in the order the merge applies the changes (deps first), not the list
        // order: a version can list a delete before the creation it depends on
        let applied = st.landing_order(&changes);
        // F2 (D-DKJS5, D-T5S9A, D-GV465): every approval path waits until each exception of the
        // version has a confirm answer. Then D-AAQ9Z: an unanswered deferral card, on a version
        // with no decision op too. Only a version that writes a decision can carry an exception,
        // so only it reads the version whole.
        let decides = changes.iter().flat_map(|c| &c.ops).any(|op| {
            matches!(op, Op::Create { block } | Op::Update { block, .. }
                if block.block_type == decision::DECISION)
        });
        // D-TF78H, O10: the approve item's required policy role holds the approving person: the
        // session's actor, who is the person a relay names
        if lander == Lander::Approver
            && let Some(role) = crate::pending::queue::approve_role(&st, &pref, &p)?
            && !crate::policy::roles::holds(&role, &s.actor)
        {
            return Err(crate::policy::roles::outside_role(
                &s.actor,
                &role,
                &pref.to_string(),
            ));
        }
        let holds = approval_holds(&st, &pref, decides)?;
        if !holds.open.is_empty() {
            return Err(exceptions_open(&pref, &holds.open));
        }
        if let Some((card, n)) = &holds.deferral {
            return Err(deferrals_open(&pref, card, *n));
        }
        // O14, O6: the stored verdict, then the lander's actor; the live policy the landing
        // records
        let policy = match lander {
            Lander::Approver => None,
            Lander::Policy => Some(policy_acceptance(&st, &pref, s, &record)?),
        };

        // 1. prospective canon: canon + the proposal's own changes
        let ops = applied_ops(&changes, &applied);
        let prospective = st.materialized(&Scope::Canon)?.with(&ops);
        let mut ids = BTreeSet::new();
        for c in &changes {
            for op in &c.ops {
                if let Op::Create { block } | Op::Update { block, .. } = op
                    && block.block_type == decision::DECISION
                {
                    ids.insert(block.id.clone());
                }
            }
        }
        let now = rfc3339_now();
        let mut accept_ops = vec![];
        let mut stale = vec![];
        let view = Scope::View(p.view.clone());
        for id in ids {
            let Some(b) = prospective.get(&id) else {
                continue;
            };
            if decision::status(b) != Some(decision::DecisionStatus::Proposed) {
                continue;
            }
            // 2. the view must not have moved on from the proposal's version
            let in_view = st.materialized(&view)?.get(&id).map(|v| v.version_hash());
            if in_view != Some(b.version_hash()) {
                stale.push(RdyError::new(
                    E_STALE_BASE,
                    format!(
                        "decision {id} changed in view {:?} after the proposal was made",
                        p.view
                    ),
                    format!("the view has later edits to {id}; propose again to include them"),
                ));
                continue;
            }
            let mut acc = b.clone();
            acc.fields
                .insert("status".into(), FieldValue::Str("accepted".into()));
            acc.fields
                .insert("decided_at".into(), FieldValue::Str(now.clone()));
            accept_ops.push(Op::Update {
                base: b.version_hash(),
                block: acc,
            });
        }
        // K1 (FW-PR3): a block landed since the proposal can hold a key rdy derived for a block
        // this version creates; the acceptance change re-derives it, past the keys the view holds
        let vm = st.materialized(&view)?;
        let re = crate::keys::rederive(
            &prospective,
            &rekeyable_in(&changes, &applied, &*st.materialized(&Scope::Canon)?),
            &vm,
        );
        let accepts = accept_ops.len();
        for (id, k) in &re {
            let b = prospective
                .get(id)
                .expect("a re-derived block is in the prospective canon");
            let in_view = vm.get(id).map(|v| v.version_hash());
            if in_view != Some(b.version_hash()) {
                stale.push(RdyError::new(
                    E_STALE_BASE,
                    format!(
                        "{id} needs a new key (another block holds its key now), but it changed in view {:?} after the proposal was made",
                        p.view
                    ),
                    format!("the view has later edits to {id}; propose again to include them"),
                ));
                continue;
            }
            let mut rekeyed = b.clone();
            rekeyed
                .fields
                .insert("key".into(), FieldValue::Str(k.clone()));
            accept_ops.push(Op::Update {
                base: b.version_hash(),
                block: rekeyed,
            });
        }
        if let Some(e) = stale.into_iter().next() {
            return Err(e);
        }
        // D-TX3KH: a block this version places that would share its order key with a sibling
        // gets a key of its own, in the acceptance change. Only the version's own placed blocks
        // are re-keyed, each from its proposed version, which the view must still hold. A key
        // folds into the block's acceptance or key update, so no two updates share a base.
        let canon = st.materialized(&Scope::Canon)?;
        // D-1FT5Y: a block whose op kept its base's order key, where canon's version differs
        // from that base only by its key, lands with canon's key and the op's other fields. It
        // is no placed block, and the re-key reads the candidate with canon's keys.
        let kept = crate::landing_order::kept_places(st.reader(), &canon, &prospective, &ops)?;
        let with_kept;
        let candidate = if kept.is_empty() {
            &prospective
        } else {
            let restored: Vec<Op> = kept
                .iter()
                .map(|(id, k)| {
                    let b = prospective
                        .get(id)
                        .expect("a kept block is in the prospective canon");
                    let mut at = b.clone();
                    at.order = k.clone();
                    Op::Update {
                        base: b.version_hash(),
                        block: at,
                    }
                })
                .collect();
            with_kept = prospective.with(&restored);
            &with_kept
        };
        let mut placed = crate::landing_order::placed(&canon, &ops);
        placed.retain(|id| !kept.contains_key(id));
        let mut order_keys = crate::landing_order::rekeys(&canon, candidate, &vm, &placed)?;
        order_keys.extend(kept);
        for (id, k) in &order_keys {
            let b = prospective
                .get(id)
                .expect("a re-keyed block is in the prospective canon");
            if vm.get(id).map(|v| v.version_hash()) != Some(b.version_hash()) {
                return Err(crate::landing_order::stale_error(id, &p.view));
            }
            let mut folded = false;
            for op in &mut accept_ops {
                if let Op::Update { block, .. } = op
                    && block.id == *id
                {
                    block.order = k.clone();
                    folded = true;
                }
            }
            if !folded {
                let mut rekeyed = b.clone();
                rekeyed.order = k.clone();
                accept_ops.push(Op::Update {
                    base: b.version_hash(),
                    block: rekeyed,
                });
            }
        }

        // 3. dry run: validate the final change list before writing anything. The acceptance
        // carries the deps the store gives it when it commits it to the view (the view's own
        // producers of its bases), so the merge orders it after the changes it accepts.
        let accept = if accept_ops.is_empty() {
            None
        } else {
            let mut change = Change {
                view: p.view.clone(),
                ops: accept_ops,
                deps: vec![],
                provenance: s.provenance(),
                justification: None,
                message: acceptance_message(
                    accepts > 0,
                    !re.is_empty(),
                    !order_keys.is_empty(),
                    &p.title,
                ),
                links: ChangeLinks::default(),
                read_identity: None,
            };
            change.deps = self.store.change_deps(&change, Some(&p.view))?;
            // the acceptance change holds every decision it accepts: it must fit one record, deps
            // included, or a later read or export of the history would refuse it (FW-UP1a)
            crate::reaffirm::change_fits(
                &change,
                crate::reaffirm::ACCEPT_CHANGE,
                crate::reaffirm::ACCEPT_FIX,
            )?;
            Some(change)
        };
        let mut all = changes;
        all.extend(accept.clone());
        let (mut issues, order) = self.prospective_issues(&st, &all, Some(&p.view))?;
        if let Some(first) = issues.iter().find(|i| i.severity == Severity::Error) {
            let mut e = RdyError::new(E_VALIDATION, first.message.clone(), first.fix.clone());
            e.issues = issues;
            return Err(e);
        }
        // the version's keys were chosen against the view's base when it was proposed: a pinned
        // view's base (at or before `canon_at`), or `canon_at` for a view a landing has rebased
        // since (FW-HB1), where the view's current base would hide the re-key
        let view_base = st.view_head(&p.view).map_or(st.canon_head(), |h| h.base);
        let keyed_at = earlier_landing(&st, view_base, p.canon_at)?;
        issues.extend(self.rekeyed_warnings(&st, keyed_at, &all)?);
        let landed_ops = applied_ops(&all, &order);
        // D-6B1W1, D-14SAY: the complete report of each request the version carries is computed
        // again here, under the writer, and must match the one the request recorded and the card
        // showed; a stale one is refused, never refreshed. The acceptance change is not in the
        // log yet: one that would copy or quote the content outside the approved removal (a new
        // version of an all-versions target) is stale too, never a silently grown removal
        for r in &requests {
            fresh_removal(&st, r, accept.as_slice(), &pref)?;
        }

        // D-4JMPH: the version's complete outcome, from its recorded canon and HEADs (or, for
        // a version with no bound plan, the HEADs captured once, O19), before anything is written
        let approver = s.provenance();
        let sealing =
            crate::plan::lifecycle::prepare_landing_plan(&st, &pid, &p, &approver, holds.accepted)?;

        // 4. prepare the one write (atom1): the acceptance change, the final proposal version,
        // the snapshot, the landing, the view's rebase and the refs. Nothing is written yet.
        let req = LandRequest {
            proposal: pid,
            accept: accept.clone(),
            approver,
            alt,
            relay: opts.relay,
            evidence,
            assessment: Some(assessment),
        };
        let prepared = match policy {
            None => self.store.prepare_land(req),
            Some(policy) => self.store.prepare_land_by_policy(req, policy),
        };
        let mut store = match prepared {
            Ok(prepared) => prepared,
            Err(rdy_store::StoreError::ProposalMoved { latest, .. }) => {
                return Err(moved(&st, &pref, &latest, &p.view)?);
            }
            Err(e) => return Err(approver_stale(e, &p.view)),
        };
        // p-8D6ES: then the plan the landed final version binds (a bound version's acceptance
        // version) and the seal, in the same batch, after the landing and before its signatures
        let (bound, seal) = sealing.plans(
            store.hash(),
            store.landed_version(),
            store.writes_final_version(),
        );
        self.store
            .plan_landing(&mut store, bound.as_ref(), seal.as_ref())?;
        Ok(PreparedApproval {
            store,
            pref,
            title: p.title.clone(),
            summary: p.summary.clone(),
            changes: p.changes.len(),
            view: p.view.clone(),
            issues,
            landed_ops,
            requests,
            accept,
            chosen,
            keys: re.into_iter().map(|(id, k)| (id.to_string(), k)).collect(),
            reaffirm: None,
        })
    }

    /// The private request of a re-affirmation card's isolated landing (D-CQMA4, D-EJBPD; see
    /// [`ReaffirmLanding`]), prepared under the writer and written by [`Rdy::commit_landing`]
    /// like every approval, so the signing coordinator owns the writer's release and return. The
    /// card's complete binding stands in for the briefing on this path only. On the canon the
    /// card was read on, it builds the members' metadata-only change (the shared
    /// transformation), then, in memory and in one batch: the fresh internal view
    /// `reaffirm-<card hash, first 12 digits>`, its `Create` op, the change and its `Commit` op,
    /// the proposal (the card's title and summary), its bound plan, the landing with the person
    /// as approver and the card as evidence, the seal, and the view's `Close` op. Nothing is
    /// appended here; a refusal leaves no view, ref, proposal or landing. An existing view of
    /// that name is `E_BAD_INPUT`: an old internal view is never reused.
    pub(crate) fn prepare_reaffirm_landing(
        &mut self,
        req: ReaffirmLanding<'_>,
    ) -> Result<PreparedApproval, RdyError> {
        let ReaffirmLanding {
            card,
            approver,
            relay,
            evidence,
            reason,
        } = req;
        let bug = |what: &str| {
            RdyError::new(
                E_INTERNAL,
                format!("{}: {what}", card.r#ref),
                "report this as a bug in rdy",
            )
        };
        let Some(PlanCardBinding::Reaffirm { cause, members, .. }) = &card.plan_binding else {
            return Err(bug("the card binds no re-affirmation"));
        };
        let card_hash: Hash = card
            .card_hash
            .parse()
            .map_err(|_| bug("the card has no hash"))?;
        let st = self.read();
        let canon_at = st.canon_head();
        let view = rdy_store::reaffirm_view(&card_hash);
        if st.has_view(&view) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("reserved re-affirmation view already exists: {view}"),
                "read the fresh re-affirmation card and answer it",
            ));
        }
        let pairs: Vec<(BlockId, Hash)> =
            members.iter().map(|m| (m.id.clone(), m.version)).collect();
        let ops = crate::reaffirm::card_updates(&st, cause, &pairs, &reason)?;
        let by = approver.provenance();
        let summary = card.summary.clone().unwrap_or_default();
        let change = Change {
            view: view.clone(),
            ops,
            deps: vec![],
            provenance: by.clone(),
            justification: None,
            message: card.title.clone(),
            links: ChangeLinks::default(),
            read_identity: None,
        };
        let mut prepared =
            self.store
                .prepare_reaffirm_propose(rdy_store::plan::ReaffirmProposeRequest {
                    view: &view,
                    canon_at,
                    change,
                    title: &card.title,
                    summary: &summary,
                    by: by.clone(),
                })?;
        let version = prepared.version();
        let committed = prepared
            .fresh_change()
            .cloned()
            .ok_or_else(|| bug("the proposal has no change"))?;
        // the one change this answer writes (D-CQMA4) must fit one record, or a later read or
        // export of the history would refuse it; nothing is written yet
        crate::reaffirm::change_fits(
            &committed,
            crate::reaffirm::REAFFIRM_CHANGE,
            crate::reaffirm::CARD_CHANGE_FIX,
        )?;
        // the bound plan: the candidate's complete plan on canon, with no answer
        let pp = crate::plan::lifecycle::prepare_proposal_plan(
            &st,
            &by,
            &view,
            None,
            Some(std::slice::from_ref(&committed)),
            &[],
        )?;
        let bound = pp.plan(&view, version, by.clone());
        let bound_id = self.store.bind_plan(&mut prepared, &bound)?;
        let mut store =
            self.store
                .prepare_reaffirm_land(prepared, by.clone(), relay, Some(evidence))?;
        let ids: Vec<BlockId> = members.iter().map(|m| m.id.clone()).collect();
        let seal = crate::reaffirm::card_seal(
            &st,
            &view,
            version,
            store.hash(),
            bound_id,
            &bound,
            cause,
            &ids,
            &by,
        )?;
        self.store.plan_landing(&mut store, None, Some(&seal))?;
        self.store.close_reaffirm_view(&mut store, &by)?;
        Ok(PreparedApproval {
            store,
            pref: ProposalRef {
                version,
                stable: version,
                n: 1,
            },
            title: card.title.clone(),
            summary: Some(summary),
            changes: 1,
            view,
            issues: vec![],
            landed_ops: committed.ops,
            requests: vec![],
            accept: None,
            chosen: None,
            keys: BTreeMap::new(),
            reaffirm: Some((card.r#ref.clone(), canon_at, card.card_hash.clone())),
        })
    }

    /// The freshness check of the removal requests a prepared landing lands, run again under the
    /// writer before its commit (D-6B1W1, D-14SAY): a report that changed is
    /// `E_REDACTION_STALE` with the corrected card ([`fresh_removal`]), never refreshed.
    pub(crate) fn recheck_removals(&self, prepared: &PreparedApproval) -> Result<(), RdyError> {
        if prepared.requests.is_empty() {
            return Ok(());
        }
        let st = self.read();
        for r in &prepared.requests {
            fresh_removal(&st, r, prepared.accept.as_slice(), &prepared.pref)?;
        }
        Ok(())
    }

    /// Writes the landing `prepared` holds, with `signatures` of it in the same batch, and
    /// publishes it: what `land_with` does after its checks. A failed landing writes nothing, so
    /// the published state stays as it is.
    pub(crate) fn commit_landing(
        &mut self,
        prepared: PreparedApproval,
        signatures: &[rdy_model::signature::Signature],
    ) -> Result<LandInfo, RdyError> {
        let PreparedApproval {
            store,
            pref,
            title,
            summary,
            changes,
            view,
            issues,
            landed_ops,
            requests,
            chosen,
            keys,
            reaffirm,
            ..
        } = prepared;
        // D-CQMA4: a re-affirmation card's landing was built on the canon its card was read on;
        // canon must stand there still
        if let Some((card, canon_at, _)) = &reaffirm
            && self.store.canon_head()? != *canon_at
        {
            return Err(RdyError::new(
                E_ITEM_MOVED,
                format!("{card} changed since it was shown"),
                "show the person the fresh card and ask again",
            ));
        }
        let pid = pref.version;
        let out = match self.store.commit_land(store, signatures) {
            Ok(out) => out,
            Err(e) => return Err(approver_stale(e, &view)),
        };
        // 5. publish canon incrementally: the previous canon with the landed ops (snap1). A
        // landing that applies removal requests seals its batch with the moves computed against
        // the masked preview, then publishes a state read again in the new epoch; its failures
        // never fall back to an ordinary publish
        if !requests.is_empty() {
            self.publish_removal(&requests)?;
        } else if let Err(e) = self.publish(Delta::Landed {
            landing: out.landing,
            view: view.clone(),
            ops: landed_ops,
            rebased: out.rebased.iter().map(|r| r.view.clone()).collect(),
            closed: out.closed.clone(),
        }) {
            // the landing is written: never leave the published state behind it
            let _ = self.publish(Delta::Full);
            return Err(e);
        }
        let assurance = self
            .store
            .landing(&out.landing)?
            .assurance
            .as_str()
            .to_string();
        Ok(LandInfo {
            landing: out.landing.to_string(),
            approved: pref.to_string(),
            version: pid.to_string(),
            title,
            summary,
            changes,
            assurance,
            warnings: issues,
            rebased: out
                .rebased
                .iter()
                .map(|r| RebasedInfo {
                    view: r.view.clone(),
                    dropped: r.dropped.len(),
                    conflicts: r.conflicts.len(),
                })
                .collect(),
            chosen: chosen.filter(|_| out.alt_version.is_some()),
            closed: out.closed,
            keys,
        })
    }
}

/// The private request of a re-affirmation card's isolated landing (D-CQMA4): the bound card
/// the person answered with `They still hold`, the person (the approver of the landing), the
/// relay that carried the answer if any, the card evidence (both option labels, the chosen one
/// and the card's hash), and the validated reason. Only the card's answer builds it; there is no
/// public way to it.
pub(crate) struct ReaffirmLanding<'a> {
    pub(crate) card: &'a Item,
    pub(crate) approver: Session,
    pub(crate) relay: Option<Relay>,
    pub(crate) evidence: ApprovalEvidence,
    pub(crate) reason: crate::reaffirm::Reason,
}

/// A landing prepared under the writer by [`Rdy::prepare_landing`] and not yet written: the
/// store's prepared batch, and what the commit publishes and reports. It holds no writer guard
/// and no reference into the project.
pub(crate) struct PreparedApproval {
    pub(crate) store: rdy_store::PreparedLanding,
    pref: ProposalRef,
    title: String,
    summary: Option<String>,
    changes: usize,
    view: String,
    issues: Vec<Issue>,
    landed_ops: Vec<Op>,
    requests: Vec<Hash>,
    accept: Option<Change>,
    chosen: Option<AltRef>,
    keys: BTreeMap<String, String>,
    /// A re-affirmation card's isolated landing (D-CQMA4): the card's ref and the canon head it
    /// was read on, checked again before the commit, and the card's hash, which binds every
    /// retry of the answer. None for every other landing.
    reaffirm: Option<(String, Option<Hash>, String)>,
}

impl PreparedApproval {
    /// The proposal version this landing approves.
    pub(crate) fn pref(&self) -> ProposalRef {
        self.pref
    }

    /// The proposal version a retry of this approval must approve again (pver1): this one's,
    /// except for a re-affirmation card's landing, whose version is new in each preparation (the
    /// card binds the retry instead).
    pub(crate) fn pin(&self) -> Option<ProposalRef> {
        self.reaffirm.is_none().then_some(self.pref)
    }

    /// The hash of the re-affirmation card this landing answers: a retry of the answer binds
    /// that card, also when the first request named no hash, so a card whose members or binding
    /// moved meanwhile is `E_ITEM_MOVED` with the fresh card. None for every other landing.
    pub(crate) fn card_pin(&self) -> Option<&str> {
        self.reaffirm.as_ref().map(|(_, _, h)| h.as_str())
    }

    /// The refusal of this landing when it was prepared again for an approval first bound to
    /// `pinned`, and the proposal's latest version is now this one's: `pinned` moved (pver1). An
    /// approval never moves to a newer version on its own.
    pub(crate) fn moved_from(
        &self,
        st: &ReadState,
        pinned: &ProposalRef,
    ) -> Result<RdyError, RdyError> {
        moved(st, pinned, &self.pref.version, &self.view)
    }
}

impl ReadState {
    /// Canon's landings as `rdy log` shows them, head first (H3-C5).
    pub fn log_rows(&self) -> Result<Vec<LandingRow>, RdyError> {
        self.log_page(None, None)
    }

    /// logt1: a page of the log, newest first. `since` is a landing id (at least 6 hex
    /// characters), `landing:<id>` or `tag:<version>`: only landings after it are listed. `limit`
    /// is at least 1. `since` is resolved first, so a bad one is an error even on an empty page.
    pub fn log_page(
        &self,
        since: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<LandingRow>, RdyError> {
        if limit == Some(0) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "limit must be at least 1",
                "omit limit to list every landing",
            ));
        }
        // the page reads canon's chain once, from the head, only as far as it lists; a landing
        // id is matched against the whole chain (a prefix two landings share is an error), so
        // that walk reads the chain first and pages from what it read
        type Walk<'s> = Box<dyn Iterator<Item = Result<(Hash, LandingRecord), RdyError>> + 's>;
        let (stop, walk): (Option<Hash>, Walk<'_>) = match since {
            None => (None, Box::new(self.landing_walk())),
            Some(text) if text.starts_with("tag:") => match self.resolve_scope(text)? {
                Scope::At(h) => (Some(h), Box::new(self.landing_walk())),
                _ => {
                    return Err(RdyError::new(
                        E_INTERNAL,
                        format!("{text:?} did not resolve to a landing"),
                        "report this as a bug in rdy",
                    ));
                }
            },
            Some(text) => {
                let chain = self.landings()?;
                let ids: Vec<Hash> = chain.iter().map(|(h, _)| *h).collect();
                let stop = crate::version::match_prefix(
                    &ids,
                    text.strip_prefix("landing:").unwrap_or(text),
                )?;
                (Some(stop), Box::new(chain.into_iter().map(Ok)))
            }
        };
        let mut tags: std::collections::HashMap<String, Vec<String>> = Default::default();
        for v in self.versions()? {
            tags.entry(v.landing).or_default().push(v.name);
        }
        let mut rows = vec![];
        for item in walk {
            if limit.is_some_and(|n| rows.len() >= n) {
                break;
            }
            let (id, rec) = item?;
            if Some(id) == stop {
                break;
            }
            let tags = tags.remove(&id.to_string()).unwrap_or_default();
            rows.push(self.landing_row(id, &rec, tags)?);
        }
        Ok(rows)
    }

    fn landing_row(
        &self,
        id: Hash,
        rec: &LandingRecord,
        tags: Vec<String>,
    ) -> Result<LandingRow, RdyError> {
        let p = self.reader().proposal(&rec.proposal)?;
        let approved = match &rec.approved_version {
            Some(h) => Some(version_ref(self, h)?.to_string()),
            None => None,
        };
        Ok(LandingRow {
            landing: id.to_string(),
            prev: rec.prev.map(|h| h.to_string()),
            proposal: rec.proposal.to_string(),
            approved_version: rec.approved_version.map(|h| h.to_string()),
            approved,
            title: p.title.clone(),
            summary: p.summary.clone(),
            approved_by: rec.approved_by.actor.clone(),
            proposed_by: RelayedByRow {
                actor: p.provenance.actor.clone(),
                run: p.provenance.run.clone(),
            },
            assurance: rec.assurance.as_str().into(),
            relayed_by: rec.relayed_by.as_ref().map(|r| RelayedByRow {
                actor: r.actor.clone(),
                run: r.run.clone(),
            }),
            relay_evidence: rec.relay_evidence.as_ref().map(|e| RelayEvidenceRow {
                quoted: e.quoted.clone(),
                session_ref: e.session_ref.clone(),
                question: e.question.clone(),
            }),
            evidence: rec.evidence.as_ref().map(|e| EvidenceRow {
                kind: e.kind.as_str().into(),
                options: (!e.options.is_empty()).then(|| e.options.clone()),
                choice: e.choice.clone(),
                card_hash: e.card_hash.map(|h| h.to_string()),
            }),
            assessment: rec.assessment.map(|h| h.to_string()),
            rebased_from: rec.rebased_from.iter().map(|h| h.to_string()).collect(),
            at: rfc3339(rec.approved_by.at_ms),
            at_ms: rec.approved_by.at_ms,
            changes: p.changes.len(),
            tags,
            chosen: rec.chosen.as_ref().map(|c| AltRef {
                set: alt_label(&c.set),
                member: c.member.clone(),
            }),
        })
    }
}
