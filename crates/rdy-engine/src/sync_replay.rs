//! A pull's second batch (D-QCE8M, D-NME32, D-8ZEHP, D-3RSCW, D-HVZC9): the move of local canon
//! to the fetched hub tip, and the replay of the unpushed local landings onto it.
//!
//! The fetch ([`crate::sync_fetch::fetch`]) is the first batch: it takes the hub's records and
//! records the fetched tip. [`plan`] then reads, with no write, what the second batch does:
//!
//! - **Up to date.** Local canon holds the hub tip, and the hub did not move back past the
//!   state's `applied` tip: canon stays.
//! - **Fast forward.** The hub chain holds local canon: canon takes the hub tip.
//! - **Take** (`take`): canon takes the hub tip, with no replay, also a hub tip that local canon
//!   holds below its tip.
//! - **Replay.** The remote has usable sync state: the state's `applied` tip is on local canon,
//!   and the hub's repo id is the state's. The unpushed landings are the local landings after
//!   `applied`, less any the hub chain holds. When the hub chain does not hold `applied`, the hub
//!   replaced its canon (a force move, also one back to a landing that local canon holds): the
//!   local landings up to `applied` leave canon and never replay.
//! - **No usable state.** A pull counts no landing as unpushed (D-3RSCW). Canon takes the hub
//!   tip, and the local landings after the newest landing both chains hold go into `pull-<n>`.
//!
//! [`pull`] then writes the second batch, as one batch, and publishes once. A replay takes the
//! unpushed landings oldest first. For each landing L it writes one new change per change of L,
//! in L's order: the same ops, author (actor and run), justification, message and briefing
//! token, with `links.replayed_from` naming the source. It writes the new landing L′ with the
//! fields D-NME32 fixes (`rdy_store::Store::prepare_replay_landing`), one new ASSESSMENT of L's
//! approved version at L′'s base, and a copy of each of L's signatures with the same statement
//! and signature bytes (D-8ZEHP): no key and no agent is needed. Each L′ passes the gate again
//! at its base: the explicit candidate evaluator ([`crate::landing_check::check_landing_over`])
//! on the base and the source changes, whose ops, authors and tokens the new changes copy. A
//! policy landing needs the policy that accepts it there, which L′ names. The steps before the
//! write ([`judge`]) write nothing; the replay test ([`crate::replay_test`]) runs them too.
//!
//! The replay stops at the first landing L that:
//!
//! - conflicts with the new canon (the merge rules of an ordinary landing),
//! - fails the gate, or whose policy refuses it,
//! - names a removal request in its `checks` (D-HVZC9),
//! - has a change that holds removed content (D-FNRXT, D-3M81W): a change a removal removed, or
//!   one that produces a removed block version, or
//! - comes first, under `strict` or `reviews.require_up_to_date`.
//!
//! On a stop, canon keeps the landings replayed so far. A new view `pull-<n>`, which the caller
//! owns, gets the changes of L and of every later unpushed landing by the ordinary view replay
//! (the caller is their author), never a removal's carrier change and never a change that holds
//! removed content (D-3M81W). For each block of removed content such a change writes, the view
//! records a conflict that names the removal and holds no text, until the person writes the
//! block there again (D-FNRXT, [`crate::sync_redaction::removal_conflicts`]). The reply lists
//! each such block with the removal that removed it, and no text of it (`removed`).
//! `replaced/<old local tip>`
//! keeps the old chain readable, and the reply is [`PULLED_WITH_CONFLICTS`]. A take, a pull after
//! a hub force move and a pull with no usable state write that mark too; a clean replay writes
//! none, because `rebased_from` keeps its sources reachable.
//!
//! The same batch writes the remote's `applied` tip. A failure before the batch seals abandons
//! it: the store keeps only the fetched records and the sync state of the fetch, and the store
//! takes no other write until it opens again.
use crate::error::{E_BAD_INPUT, E_NOT_FOUND, RdyError};
use crate::landing_check::{CheckInputs, LandingCheck, PrevState, check_landing_over};
use crate::read_state::ReadState;
use crate::readiness::Candidate;
use crate::readiness::assessment::assess_wire;
use crate::sync_fetch::HubState;
use crate::views::owner_of;
use crate::{Rdy, Scope, Session};
use rdy_model::assessment::{AssessedPolicy, AssessmentV2, PolicyRef, PolicyVerdict};
use rdy_model::{Assurance, Change, ChangeLinks, Hash, LandingRecord, Proposal, StateSnapshot};
use rdy_store::StoreReader;
use rdy_store::hub_canon::{Replaced, ReplacedKind, replaced_ref};
use rdy_store::sync_state::SyncCanon;
use rdy_store::sync_stream::MAX_HISTORY_DEPTH;
use rdy_store::{ReplayLanding, ReplayMerge};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// The status of a pull that replayed every unpushed landing, or had none.
pub const PULLED: &str = "pulled";
/// The status of a pull whose replay stopped (D-QCE8M, D-HVZC9).
pub const PULLED_WITH_CONFLICTS: &str = "pulled with conflicts";
/// What the reply says of a pull with no usable sync state (D-3RSCW).
pub const NO_SYNC_STATE: &str = "the remote had no sync state";
/// The prefix of the view a stopped replay makes: `pull-<n>`.
pub const PULL_VIEW_PREFIX: &str = "pull-";
/// The most `pull-<n>` names one pull tries.
const MAX_PULL_VIEWS: u64 = 100_000;

/// Why a landing did not replay. `after_stop` is a landing after the one the replay stopped at;
/// `no_sync_state` a local landing of a pull with no usable state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Conflict,
    Gate,
    Policy,
    Removal,
    Strict,
    AfterStop,
    NoSyncState,
}

/// What a pull asks of the second batch: the remote, the hub's reply to the state step, and
/// whether it takes the hub tip without replay (`take`) or stops at the first replay
/// (`strict`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub remote: String,
    pub hub: HubState,
    pub take: bool,
    pub strict: bool,
}

/// What the second batch does ([`plan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanKind {
    /// Local canon holds the hub tip.
    UpToDate,
    /// The hub chain holds local canon.
    FastForward,
    /// Canon takes the hub tip with no replay; the local landings after the newest common
    /// landing leave canon.
    Take { left: Vec<Hash> },
    /// The unpushed landings, oldest first, replay onto the hub tip. `force` is a hub force
    /// move; `left` are the local landings after the newest common landing, oldest first.
    Replay {
        unpushed: Vec<Hash>,
        force: bool,
        left: Vec<Hash>,
    },
    /// No usable state: the local landings after the newest common landing, oldest first, go
    /// into `pull-<n>`.
    NoState { suffix: Vec<Hash> },
}

/// The plan of a pull's second batch: the hub tip, the local tip, and what moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullPlan {
    pub hub_tip: Option<Hash>,
    pub local_tip: Option<Hash>,
    pub kind: PlanKind,
}

/// One replayed change: its source and the new change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayedChange {
    pub source: String,
    pub change: String,
}

/// One replayed landing: its source L, the new landing L′, L′'s fresh assessment, its changes
/// and how many of L's signatures it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReplayedLanding {
    pub source: String,
    pub landing: String,
    pub assessment: String,
    pub changes: Vec<ReplayedChange>,
    pub signatures: usize,
}

/// A local landing that did not replay, why, and the removal requests its `checks` name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NotReplayed {
    pub landing: String,
    pub reason: StopReason,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removals: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What a pull's second batch did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PullOutcome {
    /// [`PULLED`] or [`PULLED_WITH_CONFLICTS`].
    pub status: String,
    /// Local canon after the pull.
    pub canon: Option<String>,
    /// The hub tip the pull applied.
    pub hub_tip: Option<String>,
    pub replayed: Vec<ReplayedLanding>,
    pub not_replayed: Vec<NotReplayed>,
    /// The local landings that left canon, oldest first; a replayed one stays reachable by its
    /// replay's `rebased_from`.
    pub left_canon: Vec<String>,
    /// The `pull-<n>` view that holds the changes that did not replay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    /// The old local tip that `replaced/` marks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<String>,
    /// True when the remote had no usable sync state ([`NO_SYNC_STATE`]).
    pub no_sync_state: bool,
    /// The blocks of the changes that `pull-<n>` does not get because they hold removed
    /// content, each with its change and the removal that removed it; never their text. The
    /// view records a conflict on each.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<RemovedBlock>,
}

/// A block of a change that holds removed content, which a pull does not replay (D-3M81W): its
/// id, the change and the removal request. It carries no text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemovedBlock {
    pub block: String,
    pub change: String,
    pub removal: String,
}

fn depth_limit() -> RdyError {
    RdyError::new(
        crate::error::E_LIMIT,
        format!("a pull walks at most {MAX_HISTORY_DEPTH} landings of one chain"),
        "run rdy fsck; a chain this long is damaged",
    )
}

/// The landings of the chain that ends at `tip`, newest first.
fn chain(reader: &StoreReader, tip: Option<Hash>) -> Result<Vec<Hash>, RdyError> {
    let mut out = vec![];
    let mut at = tip;
    while let Some(l) = at {
        if out.len() as u64 >= MAX_HISTORY_DEPTH {
            return Err(depth_limit());
        }
        out.push(l);
        at = reader.landing(&l)?.prev;
    }
    Ok(out)
}

/// The usable canon state of the pull: the state when the hub's repo id is its own and the hub
/// has canon ([`crate::sync_fetch::usable_canon_state`]), and its `applied` tip is set and on the
/// local chain.
fn usable_applied(
    reader: &StoreReader,
    req: &PullRequest,
    local: &[Hash],
) -> Result<Option<Hash>, RdyError> {
    let Some(state) = crate::sync_fetch::usable_canon_state(reader, &req.remote, &req.hub)? else {
        return Ok(None);
    };
    Ok(state.applied.filter(|a| local.contains(a)))
}

/// What the second batch of the pull `req` does on `st` (see the module docs). Writes nothing.
/// The hub tip must be held (the fetch took it): else `E_NOT_FOUND`.
pub fn plan(st: &ReadState, req: &PullRequest) -> Result<PullPlan, RdyError> {
    let reader = st.reader();
    let local_tip = reader.canon_head()?;
    let Some(hub) = req.hub.canon else {
        // a hub with no canon: nothing leaves local canon (D-3RSCW)
        return Ok(PullPlan {
            hub_tip: None,
            local_tip,
            kind: PlanKind::UpToDate,
        });
    };
    if !reader.has_kind(rdy_store::kinds::LANDING, &hub)? {
        return Err(RdyError::new(
            E_NOT_FOUND,
            format!("hub tip {hub} is not in this project"),
            "fetch the remote's canon first",
        ));
    }
    let local = chain(reader, local_tip)?;
    let plan = |kind| {
        Ok(PullPlan {
            hub_tip: Some(hub),
            local_tip,
            kind,
        })
    };
    if local_tip == Some(hub) {
        return plan(PlanKind::UpToDate);
    }
    // when local canon holds the hub tip, the hub chain is the local chain from it
    let held = local.iter().position(|l| *l == hub);
    let hub_chain = match held {
        Some(at) => local[at..].to_vec(),
        None => chain(reader, Some(hub))?,
    };
    let hub_set: HashSet<Hash> = hub_chain.iter().copied().collect();
    if local_tip.is_none_or(|t| hub_set.contains(&t)) {
        return plan(PlanKind::FastForward);
    }
    // the local landings after the newest landing both chains hold, oldest first
    let common = local.iter().position(|l| hub_set.contains(l));
    let mut left: Vec<Hash> = local[..common.unwrap_or(local.len())].to_vec();
    left.reverse();
    // a take selects the hub tip, also one that local canon holds below its tip
    if req.take {
        return plan(PlanKind::Take { left });
    }
    let applied = usable_applied(reader, req, &local)?;
    if held.is_some() && applied.is_none_or(|a| hub_set.contains(&a)) {
        // local canon holds the hub tip, and the hub did not move back past `applied`
        return plan(PlanKind::UpToDate);
    }
    let Some(applied) = applied else {
        return plan(PlanKind::NoState { suffix: left });
    };
    // the unpushed landings: after `applied`, less those the hub chain holds
    let end = local
        .iter()
        .position(|l| *l == applied || hub_set.contains(l))
        .unwrap_or(local.len());
    let mut unpushed = local[..end].to_vec();
    unpushed.reverse();
    plan(PlanKind::Replay {
        unpushed,
        force: !hub_set.contains(&applied),
        left,
    })
}

/// The first free `pull-<n>` view name, n from 1.
fn free_view(reader: &StoreReader) -> Result<String, RdyError> {
    for n in 1..=MAX_PULL_VIEWS {
        let name = format!("{PULL_VIEW_PREFIX}{n}");
        if reader.view_head(&name)?.is_none() {
            return Ok(name);
        }
    }
    Err(RdyError::new(
        crate::error::E_LIMIT,
        format!("a pull names at most {MAX_PULL_VIEWS} pull views"),
        "close or remove old pull views, then pull again",
    ))
}

/// The new changes of landing `source` (its record `l`), replayed in L's order. A dep on an
/// earlier change of this pull's replays names its replay; any other dep is kept.
fn replayed_changes(
    reader: &StoreReader,
    source: &Hash,
    l: &LandingRecord,
    new_of: &HashMap<Hash, Hash>,
) -> Result<Vec<Change>, RdyError> {
    let mut out: Vec<Change> = Vec::with_capacity(l.changes.len());
    let mut mine: HashMap<Hash, Hash> = HashMap::new();
    for (n, c) in l.changes.iter().enumerate() {
        let src = reader.change(c)?;
        let deps = src
            .deps
            .iter()
            .map(|d| mine.get(d).or_else(|| new_of.get(d)).copied().unwrap_or(*d))
            .collect();
        let change = Change {
            view: src.view.clone(),
            ops: src.ops.clone(),
            deps,
            provenance: src.provenance.clone(),
            justification: src.justification.clone(),
            message: src.message.clone(),
            links: ChangeLinks {
                intent: None,
                alternative: src.links.alternative.clone(),
                client_nonce: Some(format!("pull:{source}:{n}")),
                imported_by: None,
                replayed_from: Some(*c),
                briefing: src.links.briefing,
            },
            read_identity: None,
        };
        mine.insert(*c, change.id());
        out.push(change);
    }
    Ok(out)
}

/// The policy version that accepts landing `l` on the candidate state `cst`, from the
/// evaluator's verdict `check`: the live policy's, when its verdict accepts and the accepting
/// rule's actors match `l`'s approver (O6). None otherwise.
fn accepting_policy(
    cst: &ReadState,
    l: &LandingRecord,
    check: &LandingCheck,
) -> Result<Option<PolicyRef>, RdyError> {
    let Some(AssessedPolicy {
        verdict: PolicyVerdict::Accepts { rule },
        ..
    }) = &check.policy
    else {
        return Ok(None);
    };
    let canon = cst.materialized(&Scope::Canon)?;
    let Some(live) = crate::policy::live_policy(cst, &canon) else {
        return Ok(None);
    };
    let actor = &l.approved_by.actor;
    let accepts = live
        .fields
        .as_ref()
        .and_then(|f| f.rules.iter().find(|r| &r.name == rule))
        .is_some_and(|r| {
            r.actors
                .iter()
                .any(|p| rdy_schema::policy::pattern_matches(p, actor))
        });
    Ok(accepts.then(|| live.reference()))
}

/// How one landing's replay ended.
enum Step {
    Replayed(ReplayedLanding, Vec<(Hash, Hash)>),
    Stopped(StopReason, Option<String>),
}

/// Test hook: a pull fails after it has written this many replayed landings (a failure
/// inside the second batch, before its seal).
#[doc(hidden)]
pub fn fail_pull_after_for_tests(n: Option<usize>) {
    FAIL_AFTER.with(|c| c.set(n));
}

thread_local! {
    static FAIL_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The removal requests that stop landing `l`'s replay (D-HVZC9, D-FNRXT): those its `checks`
/// name, else those that removed content a change of it holds. Empty when it replays.
pub(crate) fn removal_requests(
    reader: &StoreReader,
    l: &LandingRecord,
) -> Result<Vec<Hash>, RdyError> {
    if !l.checks.is_empty() {
        return Ok(l.checks.clone());
    }
    crate::sync_redaction::removed_in(reader, l)
}

/// What the judging steps of one landing's replay found, before any write: the steps a pull
/// runs and the replay test runs ([`crate::replay_test`]). `C` is what the caller's merge names
/// of a conflict.
pub(crate) enum Judged<C> {
    /// The landing names a removal request, or a change of it holds removed content: the
    /// removal requests ([`removal_requests`]).
    Removal(Vec<Hash>),
    /// A new change conflicts with the state the landing replays onto.
    Conflict(C),
    /// The new landing fails the gate at its base (`pass` is false), or readiness gives no
    /// assessment of it.
    Gate(LandingCheck),
    /// A policy landing that no live policy at its base accepts.
    Policy(LandingCheck),
    /// The landing replays.
    Replays(Box<Judgement>),
}

/// What a landing that replays gives: the new changes, the state they merge to, the gate's
/// check at the base, the fresh assessment of the approved version at the base and the policy
/// that accepts it.
pub(crate) struct Judgement {
    pub(crate) changes: Vec<Change>,
    pub(crate) state: StateSnapshot,
    pub(crate) check: LandingCheck,
    pub(crate) assessment: AssessmentV2,
    pub(crate) policy: Option<PolicyRef>,
}

/// The judging steps of the replay of landing `source` (its record `l`) onto `prev`, oldest
/// first, writing nothing: the removal stop; the new changes ([`replayed_changes`], deps through
/// `new_of`); `merge`, the caller's merge of them onto `prev` (the new state, or what it names
/// of a conflict); the gate at the base, the shared evaluator on `prev` and the source changes,
/// whose ops, authors and tokens the new changes copy; the accepting policy of a policy
/// landing; and the fresh assessment of the approved version at the base.
pub(crate) fn judge<C>(
    st: &ReadState,
    inputs: &CheckInputs,
    source: &Hash,
    l: &LandingRecord,
    new_of: &HashMap<Hash, Hash>,
    prev: &PrevState,
    merge: impl FnOnce(&[Change]) -> Result<Result<StateSnapshot, C>, RdyError>,
) -> Result<Judged<C>, RdyError> {
    let reader = st.reader();
    let removals = removal_requests(reader, l)?;
    if !removals.is_empty() {
        return Ok(Judged::Removal(removals));
    }
    // 1. the new changes and the merge onto the base
    let changes = replayed_changes(reader, source, l, new_of)?;
    let state = match merge(&changes)? {
        Ok(state) => state,
        Err(conflict) => return Ok(Judged::Conflict(conflict)),
    };
    // 2. the gate at the new base: the shared evaluator on the base and the source changes
    let candidate_landing = LandingRecord {
        prev: prev.at,
        read_identity: None,
        ..l.clone()
    };
    let check = check_landing_over(st, prev, &candidate_landing, inputs)?;
    if !check.pass {
        return Ok(Judged::Gate(check));
    }
    // 3. the fresh assessment of the approved version at the base, and the accepting policy
    let version = l.approved_version.unwrap_or(l.proposal);
    let pref = crate::gate::version_ref(st, &version)?;
    let stored = reader.proposal(&version)?;
    let proposal = Proposal {
        canon_at: prev.at,
        changes: l.changes.clone(),
        ..stored
    };
    let cst = st.candidate_state_over(prev, &proposal.view, &l.changes, inputs)?;
    let policy = if l.assurance == Assurance::Policy || l.policy.is_some() {
        match accepting_policy(&cst, l, &check)? {
            Some(p) => Some(p),
            None => return Ok(Judged::Policy(check)),
        }
    } else {
        None
    };
    let c = Candidate::stored(&cst, pref, proposal)?;
    let (_, wire) = assess_wire(&cst, &c, &inputs.gate_config())?;
    let Some(mut assessment) = wire else {
        return Ok(Judged::Gate(check));
    };
    assessment.base = prev.at;
    Ok(Judged::Replays(Box::new(Judgement {
        changes,
        state,
        check,
        assessment,
        policy,
    })))
}

/// Replays landing `source` onto canon now (the open batch's), or tells why it stops: the
/// judging steps ([`judge`]) with the store's merge onto canon now, then the write.
fn replay_one(
    rdy: &mut Rdy,
    st: &ReadState,
    inputs: &CheckInputs,
    source: &Hash,
    new_of: &HashMap<Hash, Hash>,
) -> Result<Step, RdyError> {
    let l = st.reader().landing(source)?;
    let prev = PrevState::stored(st, rdy.store.canon_head()?)?;
    let merge = |changes: &[Change]| {
        Ok(match rdy.store.replay_merge(changes)? {
            ReplayMerge::Clean(state) => Ok(state),
            ReplayMerge::Conflict(message) => Err(message),
        })
    };
    let judged = match judge(st, inputs, source, &l, new_of, &prev, merge)? {
        Judged::Removal(_) => return Ok(Step::Stopped(StopReason::Removal, None)),
        Judged::Conflict(message) => {
            return Ok(Step::Stopped(StopReason::Conflict, Some(message)));
        }
        Judged::Gate(check) => {
            let codes: Vec<&str> = check.rows.iter().map(|i| i.code).collect();
            let detail = (!check.pass).then(|| codes.join(", "));
            return Ok(Step::Stopped(StopReason::Gate, detail));
        }
        Judged::Policy(_) => return Ok(Step::Stopped(StopReason::Policy, None)),
        Judged::Replays(j) => *j,
    };
    // 4. the new landing, into the open batch
    let prepared = rdy.store.prepare_replay_landing(
        ReplayLanding {
            source: *source,
            changes: judged.changes,
            assessment: judged.assessment,
            policy: judged.policy,
        },
        judged.state,
    )?;
    let ids: Vec<(Hash, Hash)> = l
        .changes
        .iter()
        .copied()
        .zip(prepared.changes().iter().copied())
        .collect();
    let replayed = ReplayedLanding {
        source: source.to_string(),
        landing: prepared.hash().to_string(),
        assessment: prepared.assessment().to_string(),
        changes: ids
            .iter()
            .map(|(s, n)| ReplayedChange {
                source: s.to_string(),
                change: n.to_string(),
            })
            .collect(),
        signatures: prepared.signatures(),
    };
    rdy.store.commit_replay_landing(prepared)?;
    Ok(Step::Replayed(replayed, ids))
}

/// The changes of `landings` that a `pull-<n>` view replays: each landing's changes in order,
/// without a removal's carrier change (D-HVZC9) and without a change that holds removed
/// content (D-3M81W), whose blocks `removed` gains, each with its removal, and `marks` each
/// with the removed version it writes, once per block (D-FNRXT: the view's conflicts).
fn remainder_changes(
    reader: &StoreReader,
    landings: &[Hash],
    removed: &mut Vec<RemovedBlock>,
    marks: &mut Vec<crate::sync_redaction::RemovalMark>,
) -> Result<Vec<Hash>, RdyError> {
    let mut out = vec![];
    let mut seen = HashSet::new();
    for l in landings {
        for c in reader.landing(l)?.changes {
            if reader.redaction_for_carrier(&c)?.is_some() || !seen.insert(c) {
                continue;
            }
            let blocks = crate::sync_redaction::removed_ops(reader, &c)?;
            if blocks.is_empty() {
                out.push(c);
                continue;
            }
            for m in blocks {
                removed.push(RemovedBlock {
                    block: m.block.to_string(),
                    change: c.to_string(),
                    removal: m.removal.to_string(),
                });
                if !marks.iter().any(|x| x.block == m.block) {
                    marks.push(m);
                }
            }
        }
    }
    Ok(out)
}

/// Makes the `pull-<n>` view on canon now, owned by `s`, replays `sources` into it by the
/// ordinary view replay, then commits one text-free mark per block of `marks`
/// ([`crate::sync_redaction::mark_change`]), in the open batch. Returns its name.
fn remainder_view(
    rdy: &mut Rdy,
    s: &Session,
    st: &ReadState,
    inputs: &CheckInputs,
    sources: &[Hash],
    marks: &[crate::sync_redaction::RemovalMark],
) -> Result<String, RdyError> {
    let name = free_view(&rdy.store.reader())?;
    let tip = rdy.store.canon_head()?;
    let by = s.provenance();
    let head = rdy
        .store
        .view_create(&name, tip, Some(owner_of(s)), false, &by)?;
    if !sources.is_empty() {
        // the new view as the replay reads it: on canon now, holding nothing yet
        let vst = st.candidate_state(tip, &name, &[], inputs);
        let (prepared, _) = crate::views::replay::replay_changes(
            &vst,
            &name,
            &head.head.to_string(),
            sources,
            &by,
        )?;
        rdy.store.view_replay_prepared(&name, &prepared, &by)?;
    }
    if !marks.is_empty() {
        let changes: Vec<Change> = marks
            .iter()
            .map(|m| crate::sync_redaction::mark_change(&name, m, &by))
            .collect();
        rdy.store
            .commit_many(&name, changes, &|_| None, &[], None, &by)?;
    }
    Ok(name)
}

/// The second batch of the pull `req` by `s`, after its fetch (see the module docs). The remote
/// must have the canon sync state the fetch writes (`E_BAD_INPUT` otherwise). The batch writes
/// the canon move, the replays, the `pull-<n>` view, the `replaced/` mark and the remote's
/// `applied` tip, and publishes once. A failure before the seal abandons the batch.
pub fn pull(rdy: &mut Rdy, s: &Session, req: &PullRequest) -> Result<PullOutcome, RdyError> {
    if rdy.is_holder() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "this store is a holder",
            "pull into a project",
        ));
    }
    rdy.seal_open()?;
    let st = rdy.read();
    let state = st.reader().sync_canon(&req.remote)?.ok_or_else(|| {
        RdyError::new(
            E_BAD_INPUT,
            format!("remote {} has no fetched canon", req.remote),
            "fetch the remote's canon first",
        )
    })?;
    let plan = plan(&st, req)?;
    let unchanged = plan.kind == PlanKind::UpToDate
        && (plan.hub_tip.is_none() || state.applied == plan.hub_tip);
    if unchanged {
        return Ok(PullOutcome {
            status: PULLED.into(),
            canon: plan.local_tip.map(|h| h.to_string()),
            hub_tip: plan.hub_tip.map(|h| h.to_string()),
            replayed: vec![],
            not_replayed: vec![],
            left_canon: vec![],
            view: None,
            replaced: None,
            no_sync_state: false,
            removed: vec![],
        });
    }
    let inputs = CheckInputs::of_state(&st);
    match second_batch(rdy, s, &st, &inputs, req, &plan, state) {
        Ok(outcome) => {
            drop(st);
            rdy.publish_chain_moved()?;
            Ok(outcome)
        }
        Err(e) => {
            drop(st);
            rdy.store.abandon_open_batch();
            Err(e)
        }
    }
}

/// The writes of the second batch, into the open batch (see [`pull`]).
fn second_batch(
    rdy: &mut Rdy,
    s: &Session,
    st: &ReadState,
    inputs: &CheckInputs,
    req: &PullRequest,
    plan: &PullPlan,
    state: SyncCanon,
) -> Result<PullOutcome, RdyError> {
    let reader = st.reader();
    let mut out = PullOutcome {
        status: PULLED.into(),
        canon: None,
        hub_tip: plan.hub_tip.map(|h| h.to_string()),
        replayed: vec![],
        not_replayed: vec![],
        left_canon: vec![],
        view: None,
        replaced: None,
        no_sync_state: false,
        removed: vec![],
    };
    // the landings that go into `pull-<n>`, and whether the old tip is marked replaced
    let mut remainder: Vec<Hash> = vec![];
    let mut mark = false;
    if let Some(hub) = plan.hub_tip
        && plan.kind != PlanKind::UpToDate
    {
        rdy.store.move_project_canon(hub)?;
    }
    match &plan.kind {
        PlanKind::UpToDate | PlanKind::FastForward => {}
        PlanKind::Take { left } => {
            out.left_canon = strings(left);
            mark = true;
        }
        PlanKind::NoState { suffix } => {
            out.left_canon = strings(suffix);
            out.no_sync_state = true;
            for l in suffix {
                out.not_replayed
                    .push(not_replayed(reader, l, StopReason::NoSyncState, None)?);
            }
            remainder = suffix.clone();
            mark = true;
            if !suffix.is_empty() {
                // as for a stopped replay: the local landings wait in `pull-<n>`
                out.status = PULLED_WITH_CONFLICTS.into();
            }
        }
        PlanKind::Replay {
            unpushed,
            force,
            left,
        } => {
            out.left_canon = strings(left);
            mark = *force;
            let strict = req.strict || inputs.require_up_to_date;
            let mut new_of: HashMap<Hash, Hash> = HashMap::new();
            for (i, l) in unpushed.iter().enumerate() {
                let step = if strict {
                    Step::Stopped(StopReason::Strict, None)
                } else {
                    replay_one(rdy, st, inputs, l, &new_of)?
                };
                match step {
                    Step::Replayed(r, ids) => {
                        new_of.extend(ids);
                        out.replayed.push(r);
                        if FAIL_AFTER.with(|c| c.get()) == Some(out.replayed.len()) {
                            return Err(RdyError::new(
                                crate::error::E_INTERNAL,
                                "injected pull failure",
                                "a test hook",
                            ));
                        }
                    }
                    Step::Stopped(reason, detail) => {
                        out.not_replayed
                            .push(not_replayed(reader, l, reason, detail)?);
                        for later in &unpushed[i + 1..] {
                            out.not_replayed.push(not_replayed(
                                reader,
                                later,
                                StopReason::AfterStop,
                                None,
                            )?);
                        }
                        remainder = unpushed[i..].to_vec();
                        mark = true;
                        out.status = PULLED_WITH_CONFLICTS.into();
                        break;
                    }
                }
            }
        }
    }
    let tip = rdy.store.canon_head()?;
    if !remainder.is_empty() {
        let mut marks = vec![];
        let sources = remainder_changes(reader, &remainder, &mut out.removed, &mut marks)?;
        out.view = Some(remainder_view(rdy, s, st, inputs, &sources, &marks)?);
    }
    let mut refs = vec![];
    if mark && let (Some(old), Some(by)) = (plan.local_tip, tip) {
        let value = Replaced {
            kind: ReplacedKind::Landing,
            by,
            at_ms: s.provenance().at_ms,
        };
        refs.push((replaced_ref(&old), value.encode()));
        out.replaced = Some(old.to_string());
    }
    let canon = SyncCanon {
        applied: plan.hub_tip.or(state.applied),
        ..state
    };
    rdy.store.write_pull_refs(&req.remote, canon, refs)?;
    out.canon = tip.map(|h| h.to_string());
    Ok(out)
}

/// The reply row of a landing `l` that did not replay.
fn not_replayed(
    reader: &StoreReader,
    l: &Hash,
    reason: StopReason,
    detail: Option<String>,
) -> Result<NotReplayed, RdyError> {
    let removals = if reason == StopReason::Removal {
        strings(&removal_requests(reader, &reader.landing(l)?)?)
    } else {
        vec![]
    };
    Ok(NotReplayed {
        landing: l.to_string(),
        reason,
        removals,
        detail,
    })
}

fn strings(hashes: &[Hash]) -> Vec<String> {
    hashes.iter().map(Hash::to_string).collect()
}
