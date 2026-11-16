//! Views that travel with a sync (D-4E32E, D-XNQ6M, D-3RSCW, D-47HVF): the identity of a pushed
//! view, what a push sends and refuses, and what a pull does to each view it names.
//!
//! - **Identity.** A pushed view's identity is its name in the hub repo, guarded by its root: the
//!   hash of its chain's first op. Each op holds its view's name, so a pulled view keeps its name.
//! - **Per machine.** A head's `owner` and `pinned` never travel. A push carries each op as it
//!   is, `Own` and the owners in `Create` and `Fork` too. A pull never changes this machine's
//!   owner or pin, and a view new to this machine arrives with no owner and unpinned. An undo
//!   that sets the owner is refused in a view with a hub head ([`has_hub_head`]).
//! - **Selection.** No view travels unless the call names it, and a name given twice is
//!   refused ([`unique_names`]). Push sends canon first, then the named views, a fork after its
//!   source ([`push_plan`]). Pull handles canon first, then the named views ([`pull_views`]).
//! - **Push.** [`push_plan`] checks each named view and says what its push sends: the ops after
//!   the hub's head, or from the root for a first push. A view whose root is not the hub view's,
//!   and a fork whose source view the hub does not hold and the call does not name, are refused.
//! - **Pull.** [`pull_views`] runs after the fetch of each view. A new view takes the hub's
//!   head. A local view with another root is refused (`E_VIEW_EXISTS`). When the hub's head
//!   extends the local one, the view moves to it. When both moved, pull re-chains the unpushed
//!   local ops after the hub's head: the ops after the last hub head this machine knew for the
//!   view (the state's `applied` head, or the newest op both chains hold). An `Undo` of a
//!   re-chained op names its new id, local `Rebase` ops are dropped and reported, the old head
//!   stays readable under `replaced/` (kind view op), and then the view rebase rule runs. A
//!   re-chained op that conflicts is a conflict of the view, as an include records one. A
//!   record that names an unpushed op, or an open proposal of the view, refuses that view alone
//!   (`E_VIEW_DIVERGED`): canon and the other views still pull.
//! - **Take.** With `take`, each named view moves to the hub's head. The old local chain stays
//!   readable under `replaced/`, its changes stay available to include, and the local owner and
//!   pin stay.
//!
//! The pull writes each view's move and its sync state (`applied` set to the hub's head) into
//! one batch, and publishes once. A refusal is decided before its view writes anything.
use crate::error::*;
use crate::read_state::ReadState;
use crate::sync_fetch::{HubState, usable_view_state};
use crate::views::{ConflictInfo, conflict_infos, no_view};
use crate::{Rdy, Scope, Session};
use rdy_model::Hash;
use rdy_store::StoreReader;
use rdy_store::hub_canon::{Replaced, ReplacedKind};
use rdy_store::sync_state::SyncView;
use rdy_store::view::{ViewHead, ViewOpKind, ViewReferrer};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// `template` with each placeholder of `values` filled in.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in values {
        out = out.replace(k, v);
    }
    out
}

/// `E_BAD_INPUT` for view `name` named twice (D-47HVF `SYNC_VIEW_TWICE`).
pub fn view_twice(name: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        fill(SYNC_VIEW_TWICE_MESSAGE, &[("<name>", name)]),
        SYNC_VIEW_TWICE_FIX,
    )
}

/// `E_NOT_FOUND` for a pull of view `name` that remote `remote` lacks (D-47HVF
/// `SYNC_VIEW_MISSING`).
pub fn view_missing(name: &str, remote: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        fill(
            SYNC_VIEW_MISSING_MESSAGE,
            &[("<name>", name), ("<remote>", remote)],
        ),
        SYNC_VIEW_MISSING_FIX,
    )
}

/// `E_VIEW_EXISTS` for a pull that meets local view `name` with root `local` while the hub's
/// root is `hub` (D-47HVF `SYNC_VIEW_EXISTS`).
pub fn view_exists(name: &str, local: &Hash, hub: &Hash) -> RdyError {
    let (local, hub) = (local.to_string(), hub.to_string());
    let values = [("<name>", name), ("<local>", &local), ("<hub>", &hub)];
    RdyError::new(
        E_VIEW_EXISTS,
        fill(SYNC_VIEW_EXISTS_MESSAGE, &values),
        fill(SYNC_VIEW_EXISTS_FIX, &values),
    )
}

/// `E_VIEW_EXISTS` for a push of view `name` with root `local` while the hub's root is `hub`
/// (D-47HVF `SYNC_VIEW_ROOT`).
pub fn view_root(name: &str, local: &Hash, hub: &Hash) -> RdyError {
    let (local, hub) = (local.to_string(), hub.to_string());
    let values = [("<name>", name), ("<local>", &local), ("<hub>", &hub)];
    RdyError::new(
        E_VIEW_EXISTS,
        fill(SYNC_VIEW_ROOT_MESSAGE, &values),
        SYNC_VIEW_ROOT_FIX,
    )
}

/// `E_VIEW_DIVERGED` for view `name` whose unpushed work `record` names (D-47HVF
/// `SYNC_VIEW_DIVERGED`).
pub fn view_diverged(name: &str, record: &str) -> RdyError {
    let values = [("<name>", name), ("<record>", record)];
    RdyError::new(
        E_VIEW_DIVERGED,
        fill(SYNC_VIEW_DIVERGED_MESSAGE, &values),
        fill(SYNC_VIEW_DIVERGED_FIX, &values),
    )
}

/// `E_BAD_INPUT` for a push of view `name` that forks from view `source`, which the hub does not
/// hold (D-47HVF `SYNC_FORK_SOURCE`).
pub fn fork_source(name: &str, source: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        fill(
            SYNC_FORK_SOURCE_MESSAGE,
            &[("<name>", name), ("<source>", source)],
        ),
        SYNC_FORK_SOURCE_FIX,
    )
}

/// `E_BAD_INPUT` for an undo that would set the owner of view `name`, which has a hub head
/// (D-47HVF `SYNC_UNDO_OWNER`).
pub fn undo_owner(name: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        fill(SYNC_UNDO_OWNER_MESSAGE, &[("<name>", name)]),
        SYNC_UNDO_OWNER_FIX,
    )
}

/// The `<record>` of `E_VIEW_DIVERGED` for a record that names an unpushed op.
fn record_text(r: &ViewReferrer) -> String {
    match r {
        ViewReferrer::Answer(h) => format!("answer {}", h.short()),
        ViewReferrer::Fork { view, .. } => format!("view {view}"),
        ViewReferrer::AltSet(h) => format!("alternative set {}", h.short()),
    }
}

/// Refuses a list of view names that names a view twice (D-47HVF).
pub fn unique_names(names: &[String]) -> Result<(), RdyError> {
    let mut seen = BTreeSet::new();
    for n in names {
        if !seen.insert(n.as_str()) {
            return Err(view_twice(n));
        }
    }
    Ok(())
}

/// Whether view `name`, whose local head is `head`, has a hub head: a remote's sync state for it
/// holds a fetched or applied head, and its root is the local chain's (D-4E32E).
pub fn has_hub_head(reader: &StoreReader, name: &str, head: &Hash) -> Result<bool, RdyError> {
    let mut root = None;
    for remote in reader.sync_remotes()? {
        let Some(v) = reader.sync_view(&remote, name)? else {
            continue;
        };
        if v.fetched.is_none() && v.applied.is_none() {
            continue;
        }
        let local = match root {
            Some(r) => r,
            None => *root.insert(reader.view_chain_root(name, head)?),
        };
        if v.root == local {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A hub view as the hub's state step reports it: its root and its head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubView {
    pub root: Hash,
    pub head: Hash,
}

/// What a push sends for one view: its root, its local head, and the hub's head it extends
/// (`base`; None for a first push, which sends the chain from the root). `holds_hub_head` is
/// false when the local chain does not hold the hub's head: the hub then refuses the move, and a
/// pull comes first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewPush {
    pub view: String,
    pub root: Hash,
    pub head: Hash,
    pub base: Option<Hash>,
    pub holds_hub_head: bool,
}

/// The views a push sends, in the order it sends them (D-4E32E, D-47HVF): `names` as given, each
/// fork after a named source. `hub` holds each view the hub repo holds. Refused, before any
/// write: a name given twice (`E_BAD_INPUT`), a view this project lacks (`E_NOT_FOUND`, the
/// unknown-view refusal), a view whose root is not the hub view's (`E_VIEW_EXISTS`), and a fork
/// whose source view the hub does not hold and `names` does not name (`E_BAD_INPUT`).
pub fn push_plan(
    st: &ReadState,
    names: &[String],
    hub: &BTreeMap<String, HubView>,
) -> Result<Vec<ViewPush>, RdyError> {
    unique_names(names)?;
    let reader = st.reader();
    let named: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut plans: Vec<(ViewPush, Option<String>)> = vec![];
    for name in names {
        let head = st.view_head(name).ok_or_else(|| no_view(name))?.head;
        let chain = reader.view_chain(name, &head)?;
        let (root, first) = chain.last().expect("a chain has a root");
        let hub_view = hub.get(name);
        if let Some(h) = hub_view
            && h.root != *root
        {
            return Err(view_root(name, root, &h.root));
        }
        let source = match &first.op {
            ViewOpKind::Fork { from, .. } => {
                if !hub.contains_key(from) && !named.contains(from.as_str()) {
                    return Err(fork_source(name, from));
                }
                Some(from.clone()).filter(|f| named.contains(f.as_str()))
            }
            _ => None,
        };
        let base = hub_view.map(|h| h.head);
        let holds_hub_head = base.is_none_or(|b| chain.iter().any(|(h, _)| *h == b));
        plans.push((
            ViewPush {
                view: name.clone(),
                root: *root,
                head,
                base,
                holds_hub_head,
            },
            source,
        ));
    }
    // each fork after its named source; otherwise the order the call gave
    let mut out: Vec<ViewPush> = vec![];
    let mut sent: BTreeSet<String> = BTreeSet::new();
    while !plans.is_empty() {
        let at = plans
            .iter()
            .position(|(p, s)| s.as_ref().is_none_or(|s| sent.contains(s) || *s == p.view))
            .unwrap_or(0);
        let (p, _) = plans.remove(at);
        sent.insert(p.view.clone());
        out.push(p);
    }
    Ok(out)
}

/// What a pull asks of the named views, after their fetch: the remote, the hub's reply to the
/// state step, each named view with the hub's head of it (None: the hub lacks the view), and
/// whether the pull takes the hub's heads (`take`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewPullRequest {
    pub remote: String,
    pub hub: HubState,
    pub views: Vec<(String, Option<Hash>)>,
    pub take: bool,
}

/// What a pull did to one view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewPullStatus {
    /// The view is new to this machine: it took the hub's head, with no owner and unpinned.
    New,
    /// The local chain holds the hub's head: the view stays.
    UpToDate,
    /// The hub's chain holds the local head: the view moved to the hub's head.
    FastForward,
    /// The unpushed local ops were re-chained after the hub's head.
    Rechained,
    /// The view took the hub's head (`take`).
    Taken,
    /// The view was refused; `refusal` says why.
    Refused,
}

/// One re-chained op: its old id and its new one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RechainedOp {
    pub source: String,
    pub op: String,
}

/// What a pull did to one named view.
#[derive(Clone, Debug, Serialize)]
pub struct ViewPulled {
    pub view: String,
    pub status: ViewPullStatus,
    /// The hub's head of the view.
    pub hub_head: String,
    /// The local head after the pull.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The re-chained ops, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rechained: Vec<RechainedOp>,
    /// The local `Rebase` ops the re-chain dropped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped_rebases: Vec<String>,
    /// The `Rebase` the view rebase rule wrote after the re-chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebase: Option<String>,
    /// The view's conflicts after a re-chain.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ConflictInfo>,
    /// The old local head that `replaced/` marks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<String>,
    /// True when the remote had no usable sync state for the view.
    pub no_sync_state: bool,
    /// Why the view was refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<RdyError>,
}

/// What one view's pull writes, decided before it writes.
enum Move {
    /// The view stays; only its state moves.
    Stay(ViewPullStatus),
    /// The view's ref moves to `head`; `replaced` marks the old local head.
    Set {
        status: ViewPullStatus,
        head: ViewHead,
        replaced: Option<Hash>,
    },
    /// The unpushed ops, oldest first, re-chain after `from`; `old` is the local head.
    Rechain {
        from: ViewHead,
        ops: Vec<(Hash, rdy_store::ViewOp)>,
        old: Hash,
    },
}

/// One named view's plan: the hub's head and root, whether the state was usable, and the move,
/// or the view's refusal.
struct ViewPlan {
    name: String,
    hub_head: Hash,
    root: Hash,
    no_state: bool,
    state: SyncView,
    action: Result<Move, RdyError>,
}

/// The `<record>` of the first record that names `ops`, unpushed ops of view `name`, or of an
/// open proposal of the view; None when nothing does.
fn diverged_record(st: &ReadState, name: &str, ops: &[Hash]) -> Result<Option<String>, RdyError> {
    for (pref, p) in st.open_proposals()? {
        if p.view == name {
            return Ok(Some(pref.to_string()));
        }
    }
    Ok(st
        .reader()
        .view_op_referrers(name, ops)?
        .first()
        .map(record_text))
}

/// The move of view `name` to the hub's head `hub_head`, whose chain is `hub_chain` (newest
/// first), as `req` asks; or the view's refusal. Writes nothing.
fn plan_move(
    st: &ReadState,
    req: &ViewPullRequest,
    name: &str,
    hub_head: Hash,
    hub_chain: &[(Hash, rdy_store::ViewOp)],
    applied: Option<Hash>,
) -> Result<Result<Move, RdyError>, RdyError> {
    let reader = st.reader();
    let Some(local) = st.view_head(name).cloned() else {
        let head = reader.chain_view_head(name, &hub_head, None, false)?;
        return Ok(Ok(Move::Set {
            status: ViewPullStatus::New,
            head,
            replaced: None,
        }));
    };
    if local.head == hub_head {
        return Ok(Ok(Move::Stay(ViewPullStatus::UpToDate)));
    }
    let hub_set: HashSet<Hash> = hub_chain.iter().map(|(h, _)| *h).collect();
    let moved = || reader.chain_view_head(name, &hub_head, local.owner.clone(), local.pinned);
    if hub_set.contains(&local.head) {
        return Ok(Ok(Move::Set {
            status: ViewPullStatus::FastForward,
            head: moved()?,
            replaced: None,
        }));
    }
    let chain = reader.view_chain(name, &local.head)?;
    if req.take {
        return Ok(Ok(Move::Set {
            status: ViewPullStatus::Taken,
            head: moved()?,
            replaced: Some(local.head),
        }));
    }
    let (local_root, hub_root) = (
        chain.last().expect("a root").0,
        hub_chain.last().expect("a root").0,
    );
    if local_root != hub_root {
        return Ok(Err(view_exists(name, &local_root, &hub_root)));
    }
    if chain.iter().any(|(h, _)| *h == hub_head) {
        // the local chain holds the hub's head: its later ops are unpushed, nothing to pull
        return Ok(Ok(Move::Stay(ViewPullStatus::UpToDate)));
    }
    // the unpushed ops: after the last hub head this machine knew, less those the hub holds
    let end = chain
        .iter()
        .position(|(h, _)| Some(*h) == applied || hub_set.contains(h))
        .unwrap_or(chain.len());
    let mut ops: Vec<(Hash, rdy_store::ViewOp)> = chain[..end].to_vec();
    ops.reverse();
    let ids: Vec<Hash> = ops.iter().map(|(h, _)| *h).collect();
    if let Some(record) = diverged_record(st, name, &ids)? {
        return Ok(Err(view_diverged(name, &record)));
    }
    Ok(Ok(Move::Rechain {
        from: moved()?,
        ops,
        old: local.head,
    }))
}

/// The plan of each view `req` names, in order; the whole call's refusals first: a name given
/// twice, a view the hub lacks, a hub head this project does not hold, and a view the remote
/// has no fetched state for.
fn plan_views(st: &ReadState, req: &ViewPullRequest) -> Result<Vec<ViewPlan>, RdyError> {
    let reader = st.reader();
    let names: Vec<String> = req.views.iter().map(|(n, _)| n.clone()).collect();
    unique_names(&names)?;
    let mut heads = vec![];
    for (name, head) in &req.views {
        let head = head.ok_or_else(|| view_missing(name, &req.remote))?;
        if !reader.has_kind(rdy_store::kinds::VIEW_OP, &head)? {
            return Err(RdyError::new(
                E_NOT_FOUND,
                format!("hub head {head} of view {name} is not in this project"),
                "fetch the remote's view first",
            ));
        }
        let state = reader.sync_view(&req.remote, name)?.ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("remote {} has no fetched view {name}", req.remote),
                "fetch the remote's view first",
            )
        })?;
        heads.push((name.clone(), head, state));
    }
    let mut out = vec![];
    for (name, hub_head, state) in heads {
        let usable = usable_view_state(reader, &req.remote, &name, &req.hub)?;
        let applied = usable.as_ref().and_then(|u| u.applied);
        let hub_chain = reader.view_chain(&name, &hub_head)?;
        let root = hub_chain.last().expect("a chain has a root").0;
        let action = plan_move(st, req, &name, hub_head, &hub_chain, applied)?;
        out.push(ViewPlan {
            name,
            hub_head,
            root,
            no_state: usable.is_none(),
            state,
            action,
        });
    }
    Ok(out)
}

/// The pull of the views `req` names by `s`, after their fetch (see the module docs): each view
/// moves, re-chains, takes the hub's head or is refused, and every view that is not refused
/// records the hub's head as its `applied` head. One batch, one publish; a failure before the
/// seal abandons the batch. Refused as a whole, before any write: a holder store, a name given
/// twice (`E_BAD_INPUT`), a view the hub lacks (`E_NOT_FOUND`), a hub head this project does not
/// hold, and a view with no fetched state.
pub fn pull_views(
    rdy: &mut Rdy,
    s: &Session,
    req: &ViewPullRequest,
) -> Result<Vec<ViewPulled>, RdyError> {
    if rdy.is_holder() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "this store is a holder",
            "pull into a project",
        ));
    }
    rdy.seal_open()?;
    let st = rdy.read();
    let plans = plan_views(&st, req)?;
    drop(st);
    let mut out = vec![];
    let mut wrote = false;
    let mut failed = None;
    for plan in plans {
        match write_view(rdy, s, req, plan) {
            Ok((pulled, w)) => {
                wrote |= w;
                out.push(pulled);
            }
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if let Some(e) = failed {
        rdy.store.abandon_open_batch();
        return Err(e);
    }
    if wrote {
        rdy.publish_chain_moved()?;
    }
    let st = rdy.read();
    for p in out
        .iter_mut()
        .filter(|p| p.status == ViewPullStatus::Rechained)
    {
        p.conflicts = conflict_infos(&st, &st.conflicts(&Scope::View(p.view.clone()))?)?;
    }
    Ok(out)
}

/// Writes one view's pull into the open batch: its move, then its state and `replaced/` mark.
/// Returns what it did and whether it wrote.
fn write_view(
    rdy: &mut Rdy,
    s: &Session,
    req: &ViewPullRequest,
    plan: ViewPlan,
) -> Result<(ViewPulled, bool), RdyError> {
    let mut out = ViewPulled {
        view: plan.name.clone(),
        status: ViewPullStatus::Refused,
        hub_head: plan.hub_head.to_string(),
        head: None,
        rechained: vec![],
        dropped_rebases: vec![],
        rebase: None,
        conflicts: vec![],
        replaced: None,
        no_sync_state: plan.no_state,
        refusal: None,
    };
    let action = match plan.action {
        Ok(a) => a,
        Err(e) => {
            out.refusal = Some(e);
            return Ok((out, false));
        }
    };
    let name = plan.name.as_str();
    let by = s.provenance();
    let mut replaced: Option<Hash> = None;
    let head = match action {
        Move::Stay(status) => {
            out.status = status;
            rdy.store.view_head(name)?.map(|h| h.head)
        }
        Move::Set {
            status,
            head,
            replaced: old,
        } => {
            out.status = status;
            rdy.store.view_set_head(name, &head)?;
            replaced = old;
            Some(head.head)
        }
        Move::Rechain { from, ops, old } => {
            out.status = ViewPullStatus::Rechained;
            let r = rdy.store.view_rechain(name, from, ops)?;
            out.rechained = r
                .ops
                .iter()
                .map(|(o, n)| RechainedOp {
                    source: o.to_string(),
                    op: n.to_string(),
                })
                .collect();
            out.dropped_rebases = r.dropped_rebases.iter().map(Hash::to_string).collect();
            if let Some((op, _)) = rdy.store.view_rebase_rechained(name, &by)? {
                out.rebase = Some(op.to_string());
            }
            replaced = Some(old);
            rdy.store.view_head(name)?.map(|h| h.head)
        }
    };
    let state = SyncView {
        repo_id: req.hub.repo_id.unwrap_or(plan.state.repo_id),
        root: plan.root,
        applied: Some(plan.hub_head),
        ..plan.state.clone()
    };
    if replaced.is_none()
        && head == rdy.store.view_head(name)?.map(|h| h.head)
        && state == plan.state
    {
        // nothing moved and the state holds the hub's head already
        if out.status == ViewPullStatus::UpToDate {
            out.head = head.map(|h| h.to_string());
            return Ok((out, false));
        }
    }
    let mark = match (replaced, head) {
        (Some(old), Some(by_head)) => {
            out.replaced = Some(old.to_string());
            Some((
                old,
                Replaced {
                    kind: ReplacedKind::ViewOp,
                    by: by_head,
                    at_ms: by.at_ms,
                },
            ))
        }
        _ => None,
    };
    rdy.store
        .write_view_pull_refs(&req.remote, name, state, mark)?;
    out.head = head.map(|h| h.to_string());
    Ok((out, true))
}
