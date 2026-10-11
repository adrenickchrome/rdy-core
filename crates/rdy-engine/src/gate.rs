use crate::alt::{AltPick, alt_label};
use crate::coord_graph::AltRef;
use crate::error::*;
use crate::session::rfc3339;
use crate::tensions::{SideRef, Tension, TensionInfo, TensionSeverity, TensionStatus};
use crate::views::{
    Action, BaseBlocks, ConflictInfo, conflict_infos, creators, landings_through, missing_parents,
    parse_change,
};
use crate::{Delta, Materialized, Rdy, ReadState, Scope, Session, rfc3339_now};
use rdy_model::{
    AltChoice, AltSet, AltStatus, ApprovalEvidence, Block, BlockId, Change, ChangeLinks,
    EvidenceKind, FieldValue, Hash, LandingRecord, Op, Proposal, decision,
};
use rdy_schema::{Issue, Severity, validate_block, validate_prefix};
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
    /// (`E_CONFLICT_UNRESOLVED`) until resolved in the view and proposed again.
    pub blocking: Vec<ConflictInfo>,
    /// The tensions that involve one of the version's changes, each with `refuses`: the
    /// refusing ones first (they refuse its landing, `E_BLOCKING_TENSION`; read from what the
    /// version lands, its landing form), then by id; at most `PROPOSAL_TENSIONS_MAX`. A kind
    /// judged on the net change (unjustified_change, FW-JU1) is listed only in its landing form.
    /// A tension whose blocks all carry a blocking conflict is left out: the conflict reports it.
    pub tensions: Vec<TensionInfo>,
    /// The tensions left out past `PROPOSAL_TENSIONS_MAX`.
    #[serde(skip_serializing_if = "is_zero")]
    pub tensions_omitted: usize,
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
    /// `local`, or `relayed` when an agent relayed the human's approval (FW-B9).
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
fn moved(
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
/// local approval, `text` for a relay that names none.
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
            "run rdy approve as the human; the relaying agent is named by --relayed-by",
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
    if relay.by.actor.trim().is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "relayed_by needs an actor",
            "name the relaying agent, e.g. --relayed-by agent:name",
        ));
    }
    if relay.by.run.as_deref().is_some_and(|r| r.trim().is_empty()) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "relayed_by has an empty run",
            "name the run after '/', e.g. --relayed-by agent:name/run-7, or drop the '/'",
        ));
    }
    if relay.evidence.session_ref.trim().is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the session reference is empty",
            "pass --session <ref>, or leave it out (it defaults to cli)",
        ));
    }
    match &opts.evidence {
        Some(e) if e.kind != EvidenceKind::Text => Err(RdyError::new(
            E_BAD_INPUT,
            "only a text relay is recorded from the command line",
            "quote the human's words with --evidence",
        )),
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
            fix: "link the content that carries it with decided_by, or set needs_content: false for a process ruling".into(),
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

/// Every open proposal at its latest version: each `open/<view>` ref whose latest version has
/// not landed, sorted by the ref's text (`P-<stable short>@<n>`).
pub(crate) fn open_proposals(st: &ReadState) -> Result<Vec<(ProposalRef, Proposal)>, RdyError> {
    let mut open = vec![];
    for (_, b) in st.reader().refs_with_prefix("open/")? {
        if b.len() != 32 {
            continue;
        }
        let stable = ref_hash(&b, "an open ref")?;
        let latest = latest_version(st, &stable)?;
        let p = st.reader().proposal(&latest)?;
        if landed_as(st, &p)?.is_some() {
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
    Ok(merge_on_blocks(
        &state,
        &input,
        &|v| producers.get(v).copied().flatten(),
        &|b| deleters.get(b).copied(),
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
            .filter(|t| !net(t));
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
}

/// `E_BLOCKING_TENSION` for the refusing tensions of version `pref`: one issue per tension. The
/// fix names what resolves each refusing kind ([`crate::tensions::resolve_hint`]), each distinct
/// hint once, in the order the tensions come.
fn blocking_tensions(pref: &ProposalRef, refusing: &[TensionInfo]) -> RdyError {
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
/// approver.
fn prospective(
    st: &ReadState,
    changes: &[Change],
    approving: Option<&str>,
) -> Result<(Materialized, Vec<usize>), RdyError> {
    let reader = st.reader();
    let (_, applied) = reader
        .merge_checked(&*st.canon_snapshot()?, changes)
        .map_err(|e| match approving {
            Some(view) => approver_stale(e, view),
            None => e.into(),
        })?;
    let next = st
        .materialized(&Scope::Canon)?
        .with(&applied_ops(changes, &applied));
    Ok((next, applied))
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
        let (next, applied) = prospective(st, changes, approving)?;
        Ok((self.issues_in(st, changes, &next, &applied)?, applied))
    }

    /// The issues of `next`, the canon that landing `changes` (applied at `applied`) would give.
    fn issues_in(
        &self,
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
            issues.extend(validate_block(st.pack(), b, parent));
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
            // canon may have moved since the view was written: every target must still exist
            for target in targets(b) {
                if next.get(&target).is_none() && !deleted.contains(&target) {
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
                for gone in targets(b).intersection(&deleted) {
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

    /// Proposes view `view`'s included changes (a new version of its open proposal, if any).
    pub fn propose(
        &mut self,
        s: &Session,
        view: &str,
        title: &str,
    ) -> Result<ProposalInfo, RdyError> {
        self.propose_with(s, view, title, None, None, None)
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
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot propose", s.role),
                "start the session as a writer",
            ));
        }
        let st = self.read();
        crate::views::check_view_op(&st, s, view, Action::Propose)?;
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
        let ids: Vec<Hash> = match changes {
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
                issues = self.issues_in(&st, &changes, &next, &applied)?;
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
        let mut ids = ids;
        let mut keys = BTreeMap::new();
        if let Some((mut change, re)) = rekey {
            // the store gives the change its deps again as it commits it
            change.deps.clear();
            let ops = change.ops.clone();
            let vm = st.materialized(&Scope::View(view.to_string()))?;
            let h = self
                .store
                .commit_to_view_with(view, change, &|id| vm.version_of(id))?;
            self.publish(Delta::Commit {
                view: view.to_string(),
                change: h,
                ops,
            })?;
            ids.push(h);
            keys = re.into_iter().map(|(id, k)| (id.to_string(), k)).collect();
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
        let count = ids.len();
        let chosen = alternative.as_ref().map(|a| AltRef {
            set: alt_label(&a.set),
            member: a.member.clone(),
        });
        let (id, stable, n) = match open {
            Some((stable, latest)) => {
                let id = self.store.propose_next(
                    &latest,
                    ids,
                    title,
                    summary,
                    s.provenance(),
                    alternative,
                )?;
                (id, stable, version_number(&st, &latest)? + 1)
            }
            None => {
                let id = self.store.propose_changes(
                    view,
                    title,
                    summary,
                    s.provenance(),
                    ids,
                    alternative,
                )?;
                (id, id, 1)
            }
        };
        let stored = self.store.proposal(&id)?;
        let canon_at = stored.canon_at;
        self.publish(Delta::Seq)?;
        // tgat1: the version's tensions, read after a re-key change (if any) was committed. The
        // proposal is stored and published by now: a read error leaves the tensions out and is
        // reported, as a briefing's `tensions_error` is, and the reply keeps the proposal.
        let (tensions, tensions_omitted, tensions_error) = match self.read().proposal_tensions(
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
        let pref = ProposalRef {
            version: id,
            stable,
            n,
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
            canon_at: canon_at.map(|h| h.to_string()),
            alternative: chosen,
            keys,
            tensions_error,
            coverage_error,
        })
    }

    /// Resolves a proposal reference: a version hash, `P-<prefix>` (the proposal's latest
    /// version) or `P-<prefix>@<n>` (its version `n`, counted from 1).
    pub fn resolve_proposal(&self, text: &str) -> Result<ProposalRef, RdyError> {
        resolve_in(&self.read(), text)
    }

    /// FW-HB1: the landings since version `p` was proposed (`p.canon_at`). With none, or none
    /// that wrote a block the version writes, the approval stands (the gate then validates the
    /// version on the current canon); `strict` or `reviews.require_up_to_date` refuse any.
    fn check_base_moved(
        &self,
        st: &ReadState,
        pref: &ProposalRef,
        p: &Proposal,
        changes: &[Change],
        opts: &LandOptions,
    ) -> Result<(), RdyError> {
        // head first, up to the landing the version was proposed on (every landing when that is
        // none, or not on canon's chain)
        let mut intervening = vec![];
        let mut cur = st.canon_head();
        while let Some(h) = cur {
            if Some(h) == p.canon_at {
                break;
            }
            let rec = st.reader().landing(&h)?;
            cur = rec.prev;
            intervening.push((h, rec));
        }
        if intervening.is_empty() {
            return Ok(());
        }
        let since = || {
            format!(
                "proposal {pref} moved: canon has {} landing(s) since it was proposed ({})",
                intervening.len(),
                intervening
                    .iter()
                    .map(|(h, _)| h.short())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        if opts.strict {
            return Err(RdyError::new(
                E_PROPOSAL_MOVED,
                since(),
                "approve without --strict to accept a clean rebase, or ask its author to propose again on the current canon",
            ));
        }
        if st.require_up_to_date() {
            return Err(RdyError::new(
                E_PROPOSAL_MOVED,
                since(),
                format!(
                    "this project requires proposals on the current canon (reviews.require_up_to_date): its author must propose again (rdy propose --view {}); then approve the new version",
                    p.view
                ),
            ));
        }
        let touched: BTreeSet<&BlockId> = changes
            .iter()
            .flat_map(|c| c.ops.iter().map(Op::block_id))
            .collect();
        for (l, rec) in intervening.iter().rev() {
            let mut hit = BTreeSet::new();
            for c in &rec.changes {
                for op in &st.reader().change(c)?.ops {
                    if touched.contains(op.block_id()) {
                        hit.insert(op.block_id().clone());
                    }
                }
            }
            if hit.is_empty() {
                continue;
            }
            let ids = hit.iter().map(|b| b.to_string()).collect::<Vec<_>>();
            let mut e = RdyError::new(
                E_PROPOSAL_MOVED,
                format!(
                    "proposal {pref} moved: landing {} changed {} after it was proposed",
                    l.short(),
                    ids.join(", ")
                ),
                format!(
                    "the proposal is stale: its author must update the changed blocks in view {view} (rdy view show {view} lists its conflicts) and propose again (rdy propose --view {view}); then approve the new version",
                    view = p.view
                ),
            );
            e.issues = hit
                .into_iter()
                .map(|id| Issue {
                    severity: Severity::Error,
                    code: E_PROPOSAL_MOVED,
                    message: format!("{id} changed in landing {}", l.short()),
                    fix: "propose again".into(),
                    block: Some(id),
                })
                .collect();
            return Err(e);
        }
        Ok(())
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
        if !s.role.can_land() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot land proposals", s.role),
                "ask an approver to land it",
            ));
        }
        let st = self.read();
        let pref = resolve_in(&st, proposal)?;
        let evidence = relay_evidence_kind(s, proposal, &pref, &opts)?;
        let pid = pref.version;
        let p = st.reader().proposal(&pid)?;
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
        // altg1: the set is re-checked at approval, before the base check, so a set an earlier
        // landing chose is reported as such
        let alt = landing_alt(&st, &p)?;
        let chosen = alt.as_ref().map(|a| AltRef {
            set: alt_label(&a.set),
            member: a.member.clone(),
        });
        let changes = changes_of(&st, &p.changes)?;
        linked_alts(&st, &p.view, p.alternative.as_ref(), &p.changes, &changes)?;
        // FW-HB1: canon may have moved since the version was proposed
        self.check_base_moved(&st, &pref, &p, &changes, &opts)?;
        // gconf1: the version's own changes (not the view's current set) must carry no conflict
        // on the blocks they write
        let blocking = blocking_conflicts(&st, &p.view, &p.changes)?;
        if !blocking.is_empty() {
            return Err(unresolved(&st, &p.view, &blocking)?);
        }
        // tgat1: a blocking, undismissed tension every side of which would be in canon after
        // the landing refuses it
        // the ops fold in the order the merge applies the changes (deps first), not the list
        // order: a version can list a delete before the creation it depends on. A merge that
        // refuses is left to `prospective_issues` below, which reports it.
        let applied = st.landing_order(&changes);
        let mut cache = crate::tensions::info::InfoCache::default();
        let landing = st.landing_tensions_applied(&p.view, &p.changes, &changes, &applied)?;
        let refusing: Vec<TensionInfo> = st
            .refusing_among(&p.changes, landing)
            .iter()
            .map(|t| st.tension_info_with(t, &mut cache))
            .collect();
        if !refusing.is_empty() {
            return Err(blocking_tensions(&pref, &refusing));
        }

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
                message: match (accepts, re.is_empty()) {
                    (_, true) => format!("accept decisions in {}", p.title),
                    (0, false) => format!("give new keys in {}", p.title),
                    _ => format!("accept decisions and give new keys in {}", p.title),
                },
                links: ChangeLinks::default(),
            };
            change.deps = self.store.change_deps(&change, Some(&p.view))?;
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

        // 4. land in one write (atom1): the acceptance change, the final proposal version, the
        // snapshot, the landing, the view's rebase and the refs. A failed landing writes nothing,
        // so the published state stays as it is.
        let out = match self.store.land_atomic(LandRequest {
            proposal: pid,
            accept,
            approver: s.provenance(),
            alt,
            relay: opts.relay,
            evidence,
        }) {
            Ok(out) => out,
            Err(rdy_store::StoreError::ProposalMoved { latest, .. }) => {
                return Err(moved(&st, &pref, &latest, &p.view)?);
            }
            Err(e) => return Err(approver_stale(e, &p.view)),
        };
        // 5. publish canon incrementally: the previous canon with the landed ops (snap1)
        if let Err(e) = self.publish(Delta::Landed {
            landing: out.landing,
            view: p.view.clone(),
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
            title: p.title.clone(),
            summary: p.summary.clone(),
            changes: p.changes.len(),
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
            keys: re.into_iter().map(|(id, k)| (id.to_string(), k)).collect(),
        })
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
