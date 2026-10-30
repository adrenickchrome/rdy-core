//! Views as the engine shows and changes them: create and fork (vcreate1), include with its
//! dependency closure and exclude with its dependents (incl1, excl1), the claim a commit makes
//! on an unowned view, the closed-view rules and the conflict reads. Undo and close (undo1) are
//! in `undo`, ownership and the bulk close (own1) in `own`.
use crate::Materialized;
use crate::error::*;
use crate::query::{body_in, cut_line};
use crate::read_state::ReadState;
use crate::write::{not_humans_view, writable_view};
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{BlockId, Change, Hash, Op, StateSnapshot};
use rdy_schema::{Issue, Severity};
use rdy_store::view::ViewOpKind;
use rdy_store::{Conflict, Owner, StoreError};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub(crate) mod own;
mod replay;
mod resolve;
mod undo;

pub use own::{OwnInfo, StaleClose};
pub use replay::{
    MAX_REPLAY_BYTES, MAX_REPLAY_CHANGES, MAX_REPLAY_OPS, ReplayResult, ReplayedChange,
    replay_order_for_tests,
};
pub use resolve::ResolveResult;
pub use undo::{CloseResult, UndoResult};
pub(crate) use undo::{kind_name, net_undone};

/// A view's owner as reads show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OwnerInfo {
    pub actor: String,
    pub run: Option<String>,
}

impl From<&Owner> for OwnerInfo {
    fn from(o: &Owner) -> Self {
        OwnerInfo {
            actor: o.actor.clone(),
            run: o.run.clone(),
        }
    }
}

/// A view's head as `rdy view new` and `fork` report it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewHeadInfo {
    pub view: String,
    /// The head view op.
    pub head: String,
    /// The landing the view reads on (`ReadState::effective_base`).
    pub base: Option<String>,
    pub owner: Option<OwnerInfo>,
    pub pinned: bool,
    pub closed: bool,
    /// How many changes the view includes.
    pub changes: usize,
}

/// One side of a conflict: the change that wrote it (None: canon's side, or a base version with
/// no known producer), its version (None: deleted), the landing the change landed in, if any,
/// and a one-line summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConflictSide {
    pub change: Option<String>,
    pub version: Option<String>,
    pub landed: Option<String>,
    pub summary: String,
}

/// A conflict the view's merge recorded on one block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConflictInfo {
    pub id: String,
    pub block: String,
    pub base: Option<String>,
    pub sides: Vec<ConflictSide>,
}

/// What a view op (include, exclude) did to the view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewOpResult {
    pub view: String,
    pub head: String,
    pub changes: usize,
    pub added: Vec<String>,
    pub added_deps: Vec<String>,
    pub removed: Vec<String>,
    pub cascaded: Vec<String>,
    pub conflicts: Vec<ConflictInfo>,
}

/// What a session does to a view, for the role, existence, closed and ownership rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Commit,
    Include,
    Exclude,
    Close,
    Undo,
    Propose,
    Replay,
}

impl Action {
    fn verb(self) -> &'static str {
        match self {
            Action::Commit => "commit",
            Action::Include => "include",
            Action::Exclude => "exclude",
            Action::Close => "close",
            Action::Undo => "undo",
            Action::Propose => "propose",
            Action::Replay => "replay",
        }
    }
}

/// The session as a view owner.
pub(crate) fn owner_of(s: &Session) -> Owner {
    Owner {
        actor: s.actor.clone(),
        run: s.run.clone(),
    }
}

pub(crate) fn no_view(view: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("no view {view:?}"),
        "list views with rdy view ls (MCP: rdy_view list)",
    )
}

fn no_create(s: &Session) -> Result<(), RdyError> {
    if s.role.can_write() {
        return Ok(());
    }
    Err(RdyError::new(
        E_PERMISSION,
        format!("role {:?} cannot create views", s.role),
        "start the session as a writer",
    ))
}

/// The role, existence, closed and ownership rules for `action` on `view`. A commit or an
/// include into a view with no ref creates it; a closed view takes no commit, include, exclude
/// or replay; an agent acts only in a view it owns (`check_view_access`). Propose keeps its own
/// role, existence and closed checks (a closed view takes no proposal, D-K0SS1, R2, in
/// `propose_relayed`) and takes only the ownership rule here (legacy `:` views stay proposable
/// by the human).
pub(crate) fn check_view_op(
    st: &ReadState,
    s: &Session,
    view: &str,
    action: Action,
) -> Result<(), RdyError> {
    if action == Action::Propose {
        return crate::write::check_view_access(st, view, s, action);
    }
    writable_view(view)?;
    if !s.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot {}", s.role, action.verb()),
            "start the session as a writer",
        ));
    }
    match st.view_head(view) {
        None if matches!(action, Action::Commit | Action::Include) => {}
        None => return Err(no_view(view)),
        Some(h)
            if h.closed
                && matches!(
                    action,
                    Action::Commit | Action::Include | Action::Exclude | Action::Replay
                ) =>
        {
            return Err(StoreError::ViewClosed(view.to_string()).into());
        }
        Some(_) => {}
    }
    crate::write::check_view_access(st, view, s, action)
}

/// The owner a commit into `view` records: the session, for a view the commit creates or an
/// unowned one it claims; None for an owned view. A closed view takes no commit.
pub(crate) fn commit_claim(
    st: &ReadState,
    s: &Session,
    view: &str,
) -> Result<Option<Owner>, RdyError> {
    match st.view_head(view) {
        Some(h) if h.closed => Err(StoreError::ViewClosed(view.to_string()).into()),
        None => Ok(Some(owner_of(s))),
        Some(h) if h.owner.is_none() => Ok(Some(owner_of(s))),
        Some(_) => Ok(None),
    }
}

/// The first `n` characters of `text`.
fn first_chars(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// `text` as an error message echoes a bad id: its first 80 characters, with an ellipsis when
/// there are more (a request's id can be megabytes long).
pub(crate) fn clip(text: &str) -> String {
    let head = first_chars(text, 80);
    if head.len() < text.len() {
        format!("{head}\u{2026}")
    } else {
        head.to_string()
    }
}

/// `conflicts` as reads show them: each side with its landing and a one-line summary.
pub(crate) fn conflict_infos(
    st: &ReadState,
    conflicts: &[Conflict],
) -> Result<Vec<ConflictInfo>, RdyError> {
    let reader = st.reader();
    let canon = if conflicts.is_empty() {
        None
    } else {
        Some(st.materialized(&Scope::Canon)?)
    };
    conflicts
        .iter()
        .map(|c| {
            let sides = c
                .sides
                .iter()
                .map(|side| {
                    let landed = match &side.change {
                        Some(ch) => reader.landing_of(ch)?.map(|l| l.to_string()),
                        None => None,
                    };
                    let mut summary = match &side.change {
                        Some(ch) => {
                            let change = reader.change(ch)?;
                            let what = if change.message.trim().is_empty() {
                                let block = change.ops.iter().rev().find_map(|op| match op {
                                    Op::Create { block } | Op::Update { block, .. }
                                        if block.id == c.block =>
                                    {
                                        Some(block)
                                    }
                                    _ => None,
                                });
                                match (block, &canon) {
                                    (Some(b), Some(m)) => cut_line(&body_in(b, m), 60),
                                    _ => String::new(),
                                }
                            } else {
                                change.message.clone()
                            };
                            format!("{}: {what}", change.provenance.actor)
                        }
                        None => "canon".to_string(),
                    };
                    if side.version.is_none() {
                        summary.push_str(" (deleted)");
                    }
                    Ok(ConflictSide {
                        change: side.change.map(|h| h.to_string()),
                        version: side.version.map(|h| h.to_string()),
                        landed,
                        summary,
                    })
                })
                .collect::<Result<Vec<_>, RdyError>>()?;
            Ok(ConflictInfo {
                id: c.id.to_string(),
                block: c.block.to_string(),
                base: c.base.map(|h| h.to_string()),
                sides,
            })
        })
        .collect()
}

impl ReadState {
    /// The conflict the scope records on block `id`, as reads show it.
    pub fn conflict_info(
        &self,
        scope: &Scope,
        id: &BlockId,
    ) -> Result<Option<ConflictInfo>, RdyError> {
        match self.conflict_on(scope, id)? {
            Some(c) => Ok(conflict_infos(self, &[c])?.pop()),
            None => Ok(None),
        }
    }
}

/// View `name`'s head as this state holds it.
pub(crate) fn head_info(st: &ReadState, name: &str) -> Result<ViewHeadInfo, RdyError> {
    let h = st.view_head(name).ok_or_else(|| no_view(name))?;
    Ok(ViewHeadInfo {
        view: name.to_string(),
        head: h.head.to_string(),
        base: st.effective_base(name).map(|b| b.to_string()),
        owner: h.owner.as_ref().map(OwnerInfo::from),
        pinned: h.pinned,
        closed: h.closed,
        changes: st.included(name).len(),
    })
}

impl Rdy {
    /// Creates view `name`, owned by the session: on canon's head (`base` None or `canon`),
    /// floating unless `pin`; or pinned at a version (`tag:<version>`) or a landing
    /// (`landing:<id>`).
    pub fn view_create(
        &mut self,
        s: &Session,
        name: &str,
        base: Option<&str>,
        pin: bool,
    ) -> Result<ViewHeadInfo, RdyError> {
        writable_view(name)?;
        no_create(s)?;
        not_humans_view(name, s)?;
        let st = self.read();
        if st.has_view(name) {
            return Err(StoreError::ViewExists(name.to_string()).into());
        }
        let (base, pinned) = match base {
            None | Some("canon") => (st.canon_head(), pin),
            Some(text) if text.starts_with("tag:") || text.starts_with("landing:") => {
                match st.resolve_scope(text)? {
                    Scope::At(h) => (Some(h), true),
                    _ => return Err(bad_base(name)),
                }
            }
            Some(_) => return Err(bad_base(name)),
        };
        drop(st);
        self.store
            .view_create(name, base, Some(owner_of(s)), pinned, &s.provenance())?;
        self.publish(Delta::View {
            view: name.to_string(),
        })?;
        head_info(&self.read(), name)
    }

    /// Forks view `from` (open or closed) as the new open view `name`, owned by the session.
    pub fn view_fork(
        &mut self,
        s: &Session,
        from: &str,
        name: &str,
    ) -> Result<ViewHeadInfo, RdyError> {
        writable_view(name)?;
        no_create(s)?;
        not_humans_view(name, s)?;
        let st = self.read();
        if !st.has_view(from) {
            return Err(no_view(from));
        }
        if st.has_view(name) {
            return Err(StoreError::ViewExists(name.to_string()).into());
        }
        drop(st);
        self.store
            .view_fork(from, name, Some(owner_of(s)), &s.provenance())?;
        self.publish(Delta::View {
            view: name.to_string(),
        })?;
        head_info(&self.read(), name)
    }
}

fn bad_base(name: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "base must be canon, tag:<version> or landing:<id>",
        format!("to start from a view, fork it: rdy view fork <view> {name}"),
    )
}

/// A change id as include and exclude take it: a full hash.
pub(crate) fn parse_change(s: &str) -> Result<Hash, RdyError> {
    s.parse().map_err(|_| {
        RdyError::new(
            E_BAD_INPUT,
            format!("{:?} is not a change id", clip(s)),
            "pass the id returned by rdy_write, or one listed by rdy view show",
        )
    })
}

/// The landings at or before `base` on canon's chain (none for `None`).
pub(crate) fn landings_through(
    st: &ReadState,
    base: Option<Hash>,
) -> Result<BTreeSet<Hash>, RdyError> {
    let mut out = BTreeSet::new();
    let mut cur = base;
    while let Some(h) = cur {
        out.insert(h);
        cur = st.reader().landing(&h)?.prev;
    }
    Ok(out)
}

/// Whether `change` landed on canon's chain at or before landing `base` (None: nothing is).
/// `based` holds `landings_through(base)`, read once, when a change first needs it.
fn landed_through(
    st: &ReadState,
    base: Option<Hash>,
    based: &mut Option<BTreeSet<Hash>>,
    change: &Hash,
) -> Result<bool, RdyError> {
    let Some(l) = st.reader().landing_of(change)? else {
        return Ok(false);
    };
    if based.is_none() {
        *based = Some(landings_through(st, base)?);
    }
    Ok(based.as_ref().is_some_and(|b| b.contains(&l)))
}

/// The blocks of landing `base`'s state, read once when first asked: canon's materialization
/// when `base` is canon's head (built already in practice), else the landing's snapshot.
pub(crate) struct BaseBlocks<'a> {
    st: &'a ReadState,
    base: Option<Hash>,
    canon: Option<Arc<Materialized>>,
    snap: Option<StateSnapshot>,
}

impl<'a> BaseBlocks<'a> {
    pub(crate) fn new(st: &'a ReadState, base: Option<Hash>) -> BaseBlocks<'a> {
        BaseBlocks {
            st,
            base,
            canon: None,
            snap: None,
        }
    }

    pub(crate) fn has(&mut self, id: &BlockId) -> Result<bool, RdyError> {
        if self.base.is_none() {
            return Ok(false);
        }
        if self.base == self.st.canon_head() {
            if self.canon.is_none() {
                self.canon = Some(self.st.materialized(&Scope::Canon)?);
            }
            return Ok(self.canon.as_ref().is_some_and(|m| m.get(id).is_some()));
        }
        if self.snap.is_none() {
            self.snap = Some(self.st.reader().state_of(self.base)?);
        }
        Ok(self
            .snap
            .as_ref()
            .is_some_and(|s| s.blocks.contains_key(id)))
    }
}

/// The parent rule (final review fix round 1). A change's deps cover its op bases, its deletes
/// and conflict sides, not the parent a block is created or moved under; a set of changes that
/// leaves out the change creating such a parent would put an orphan in the view, refused only
/// when it lands. For each block `changes` create or move under a parent that none of them
/// creates and that `present` does not hold: (the change, the block, the parent), each parent
/// once, in change order.
pub(crate) fn missing_parents(
    changes: &[(Hash, Change)],
    present: &mut dyn FnMut(&BlockId) -> Result<bool, RdyError>,
) -> Result<Vec<(Hash, BlockId, BlockId)>, RdyError> {
    let created: HashSet<&BlockId> = changes
        .iter()
        .flat_map(|(_, c)| &c.ops)
        .filter_map(|op| match op {
            Op::Create { block } => Some(&block.id),
            _ => None,
        })
        .collect();
    let mut asked: HashSet<&BlockId> = HashSet::new();
    let mut out = vec![];
    for (h, c) in changes {
        for op in &c.ops {
            let (Op::Create { block } | Op::Update { block, .. }) = op else {
                continue;
            };
            let Some(p) = &block.parent else { continue };
            if created.contains(p) || !asked.insert(p) || present(p)? {
                continue;
            }
            out.push((*h, block.id.clone(), p.clone()));
        }
    }
    Ok(out)
}

/// The change that creates each block id `changes` create (the first one, in order).
pub(crate) fn creators<'a>(
    changes: impl IntoIterator<Item = (Hash, &'a Change)>,
) -> HashMap<BlockId, Hash> {
    let mut out = HashMap::new();
    for (h, c) in changes {
        for op in &c.ops {
            if let Op::Create { block } = op {
                out.entry(block.id.clone()).or_insert(h);
            }
        }
    }
    out
}

/// The changes of `included`, in included order and not in `gone`, whose deps meet `gone`
/// transitively.
pub(crate) fn dependents(
    st: &ReadState,
    included: &[Hash],
    gone: &BTreeSet<Hash>,
) -> Result<Vec<Hash>, RdyError> {
    let reader = st.reader();
    let deps: Vec<Vec<Hash>> = included
        .iter()
        .map(|c| Ok(reader.change(c)?.deps))
        .collect::<Result<_, RdyError>>()?;
    let mut out_set = gone.clone();
    loop {
        let mut grew = false;
        for (c, ds) in included.iter().zip(&deps) {
            if !out_set.contains(c) && ds.iter().any(|d| out_set.contains(d)) {
                out_set.insert(*c);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    Ok(included
        .iter()
        .filter(|c| out_set.contains(c) && !gone.contains(c))
        .copied()
        .collect())
}

/// The deps `added` needs that are neither in `included`, in `added`, nor in base `base`, found
/// breadth first from `added` (the closure an include completes), in discovery order.
pub(crate) fn missing_deps(
    st: &ReadState,
    base: Option<Hash>,
    included: &BTreeSet<Hash>,
    added: &[Hash],
) -> Result<Vec<Hash>, RdyError> {
    Ok(missing_deps_rooted(st, base, included, added)?
        .into_iter()
        .map(|(d, _)| d)
        .collect())
}

/// `missing_deps`, each dep with the change of `added` whose closure first reached it.
pub(crate) fn missing_deps_rooted(
    st: &ReadState,
    base: Option<Hash>,
    included: &BTreeSet<Hash>,
    added: &[Hash],
) -> Result<Vec<(Hash, Hash)>, RdyError> {
    let reader = st.reader();
    let added_set: BTreeSet<Hash> = added.iter().copied().collect();
    let mut found: Vec<(Hash, Hash)> = vec![];
    let mut found_set: BTreeSet<Hash> = BTreeSet::new();
    let mut queue: VecDeque<(Hash, Hash)> = added.iter().map(|c| (*c, *c)).collect();
    // the landings through the base, walked once for the whole closure
    let mut based = None;
    while let Some((c, root)) = queue.pop_front() {
        for d in reader.change(&c)?.deps {
            if included.contains(&d)
                || added_set.contains(&d)
                || found_set.contains(&d)
                || landed_through(st, base, &mut based, &d)?
            {
                continue;
            }
            found_set.insert(d);
            found.push((d, root));
            queue.push_back((d, root));
        }
    }
    Ok(found)
}

/// incl1: what an include of `added` into `view` (reading on `base`, including `included`)
/// brings along, in discovery order: the deps they need that the view lacks, then, for each
/// parent a block of theirs is created or moved under that neither the view nor the include
/// has, the unlanded change that creates it, with that change's own closure, until nothing is
/// missing. A parent no unlanded change creates (one that landed after a pinned base, say)
/// refuses the include with `E_DEPS_MISSING`, one issue per parent. A dep that landed after a
/// pinned base is brought along: an include is an explicit act, so it may bring landed work into
/// a pinned view, which an undo never does on its own (`E_UNDO_LANDING` sends the agent here;
/// final review I3, kept as built pending the founder's ruling).
fn include_closure(
    st: &ReadState,
    view: &str,
    base: Option<Hash>,
    included: &BTreeSet<Hash>,
    added: &[Hash],
) -> Result<Vec<Hash>, RdyError> {
    let reader = st.reader();
    let mut deps = missing_deps(st, base, included, added)?;
    let mut changes: Vec<(Hash, Change)> = added
        .iter()
        .chain(&deps)
        .map(|h| Ok((*h, reader.change(h)?)))
        .collect::<Result<_, RdyError>>()?;
    let mut in_base = BaseBlocks::new(st, base);
    // the ids the view's own changes create, and the creators among every view's changes: each
    // read once, when a parent first needs it
    let mut in_view: Option<HashSet<BlockId>> = None;
    let mut open: Option<HashMap<BlockId, Hash>> = None;
    loop {
        let missing = missing_parents(&changes, &mut |p| {
            if in_base.has(p)? {
                return Ok(true);
            }
            if in_view.is_none() {
                let mut ids = HashSet::new();
                for h in included {
                    for op in reader.change(h)?.ops {
                        if let Op::Create { block } = op {
                            ids.insert(block.id);
                        }
                    }
                }
                in_view = Some(ids);
            }
            Ok(in_view.as_ref().is_some_and(|ids| ids.contains(p)))
        })?;
        if missing.is_empty() {
            return Ok(deps);
        }
        if open.is_none() {
            let mut seen = HashSet::new();
            let mut all = vec![];
            for name in st.view_names() {
                for h in st.included(&name).iter() {
                    if seen.insert(*h) {
                        all.push((*h, reader.change(h)?));
                    }
                }
            }
            open = Some(creators(all.iter().map(|(h, c)| (*h, c))));
        }
        let open = open.as_ref().expect("read above");
        let have: BTreeSet<Hash> = included
            .iter()
            .copied()
            .chain(changes.iter().map(|(h, _)| *h))
            .collect();
        let mut new: Vec<Hash> = vec![];
        let mut issues = vec![];
        let mut lacking: Vec<String> = vec![];
        for (c, block, p) in &missing {
            match open.get(p) {
                Some(k) if !have.contains(k) && reader.landing_of(k)?.is_none() => {
                    if !new.contains(k) {
                        new.push(*k);
                    }
                }
                _ => {
                    lacking.push(p.to_string());
                    issues.push(Issue {
                    severity: Severity::Error,
                    code: E_DEPS_MISSING,
                    block: Some(block.clone()),
                    message: format!(
                        "{} puts {block} under {p}, which view {view:?} does not have and no unlanded change creates",
                        c.short()
                    ),
                    fix: format!(
                        "create {p} in view {view} first, or include {} into a view that has {p}",
                        c.short()
                    ),
                    });
                }
            }
        }
        if !issues.is_empty() {
            let mut e = RdyError::new(
                E_DEPS_MISSING,
                format!(
                    "view {view:?} lacks {} parent(s) the included changes need: {}",
                    lacking.len(),
                    lacking.join(", ")
                ),
                "include into a view that has those parents (a fork of the view the changes were written in)",
            );
            e.issues = issues;
            return Err(e);
        }
        let more = missing_deps(st, base, &have, &new)?;
        for h in new.iter().chain(&more) {
            changes.push((*h, reader.change(h)?));
        }
        deps.extend(new);
        deps.extend(more);
    }
}

/// cyc1: refuses an include whose changes, merged with what `view` already includes, would make
/// two blocks each other's ancestors (each change alone moves one block, so neither write saw
/// the loop). Only an include that moves a block can make one, so the merge is read only then.
fn refuse_parent_cycle(
    st: &ReadState,
    view: &str,
    base: Option<Hash>,
    added: &[Hash],
    added_deps: &[Hash],
) -> Result<(), RdyError> {
    let reader = st.reader();
    let new: Vec<(Hash, Change)> = added
        .iter()
        .chain(added_deps)
        .map(|h| Ok((*h, reader.change(h)?)))
        .collect::<Result<_, RdyError>>()?;
    // the blocks the new changes move: an update whose parent differs from its base version's
    let mut moved: Vec<(Hash, BlockId)> = vec![];
    for (h, c) in &new {
        for op in &c.ops {
            if let Op::Update { base, block } = op
                && reader.get_block(base)?.parent != block.parent
            {
                moved.push((*h, block.id.clone()));
            }
        }
    }
    if moved.is_empty() {
        return Ok(());
    }
    let mut list: Vec<Change> = st
        .included(view)
        .iter()
        .map(|h| reader.change(h))
        .collect::<Result<_, _>>()?;
    list.extend(new.into_iter().map(|(_, c)| c));
    let merged = reader.merge_onto(base, &reader.state_of(base)?, &list)?;
    let Some((a, b)) =
        reader.parent_cycle_in(&merged.state, moved.iter().map(|(_, id)| id.clone()))?
    else {
        return Ok(());
    };
    let by = moved
        .iter()
        .find(|(_, id)| *id == a || *id == b)
        .map_or(added[0], |(h, _)| *h);
    Err(RdyError::new(
        E_BAD_INPUT,
        format!(
            "including {} would make {a} and {b} each other's ancestors",
            by.short()
        ),
        "exclude the change that moves one of them, or move one of them out first",
    ))
}

/// The requested ids, parsed, duplicates collapsed, in request order.
fn parse_changes(changes: &[String]) -> Result<Vec<Hash>, RdyError> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for text in changes {
        let h = parse_change(text)?;
        if seen.insert(h) {
            out.push(h);
        }
    }
    Ok(out)
}

pub(crate) fn strings(hs: &[Hash]) -> Vec<String> {
    hs.iter().map(|h| h.to_string()).collect()
}

fn shorts(hs: &[Hash]) -> String {
    hs.iter().map(|h| h.short()).collect::<Vec<_>>().join(", ")
}

/// The result of an include or exclude, read from the state after it.
fn op_result(
    st: &ReadState,
    view: &str,
    added: &[Hash],
    added_deps: &[Hash],
    removed: &[Hash],
    cascaded: &[Hash],
) -> Result<ViewOpResult, RdyError> {
    let head = st.view_head(view).ok_or_else(|| no_view(view))?.head;
    Ok(ViewOpResult {
        view: view.to_string(),
        head: head.to_string(),
        changes: st.included(view).len(),
        added: strings(added),
        added_deps: strings(added_deps),
        removed: strings(removed),
        cascaded: strings(cascaded),
        conflicts: conflict_infos(st, &st.conflicts(&Scope::View(view.to_string()))?)?,
    })
}

impl Rdy {
    /// incl1: includes `changes` (from any view) in `view`, with what they need that the view
    /// neither includes nor has in its base: their deps, and the changes that create the parents
    /// their blocks are created or moved under (`include_closure`). Changes already included are
    /// skipped; a landed change is refused. An include into a view with no ref creates it, owned
    /// by the session.
    pub fn view_include(
        &mut self,
        s: &Session,
        view: &str,
        changes: &[String],
    ) -> Result<ViewOpResult, RdyError> {
        // 1. the rules
        let st = self.read();
        check_view_op(&st, s, view, Action::Include)?;
        // 2. the requested changes: present, not landed
        if changes.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "name at least one change to include",
                format!("rdy view include {view} <change>..."),
            ));
        }
        let requested = parse_changes(changes)?;
        let reader = st.reader();
        for c in &requested {
            if !reader.has_kind(rdy_store::kinds::CHANGE, c)? {
                return Err(RdyError::new(
                    E_NOT_FOUND,
                    format!("no change {}", c.short()),
                    "check the id with rdy view show <view>",
                ));
            }
            if let Some(l) = reader.landing_of(c)? {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("change {} already landed in {}", c.short(), l.short()),
                    "it is in canon; nothing to include",
                ));
            }
        }
        // 3. what to add: the requested changes not included, then their missing deps (BFS)
        let included: BTreeSet<Hash> = st.included(view).iter().copied().collect();
        let base = st.effective_base(view);
        let added: Vec<Hash> = requested
            .into_iter()
            .filter(|c| !included.contains(c))
            .collect();
        // 4. nothing new: no write
        if added.is_empty() {
            return op_result(&st, view, &[], &[], &[], &[]);
        }
        let added_deps = include_closure(&st, view, base, &included, &added)?;
        refuse_parent_cycle(&st, view, base, &added, &added_deps)?;
        // 5. the writes: a new view is created first (its own batch and publish)
        let exists = st.has_view(view);
        drop(st);
        if !exists {
            self.store
                .view_create(view, base, Some(owner_of(s)), false, &s.provenance())?;
            self.publish(Delta::View {
                view: view.to_string(),
            })?;
        }
        let all: Vec<Hash> = added.iter().chain(&added_deps).copied().collect();
        self.store.view_append(
            view,
            vec![ViewOpKind::Include { changes: all }],
            &s.provenance(),
        )?;
        // 6. publish and report
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        op_result(&self.read(), view, &added, &added_deps, &[], &[])
    }

    /// excl1: excludes `changes` from `view`. Included changes that depend on them refuse the
    /// exclude (`E_HAS_DEPENDENTS`) unless `cascade`, which excludes them too.
    pub fn view_exclude(
        &mut self,
        s: &Session,
        view: &str,
        changes: &[String],
        cascade: bool,
    ) -> Result<ViewOpResult, RdyError> {
        // 1. the rules
        let st = self.read();
        check_view_op(&st, s, view, Action::Exclude)?;
        // 2. the requested changes: each included
        if changes.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "name at least one change to exclude",
                format!("rdy view show {view} lists its changes"),
            ));
        }
        let requested = parse_changes(changes)?;
        let included = st.included(view);
        for c in &requested {
            if !included.contains(c) {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("change {} is not in view {view:?}", c.short()),
                    format!("rdy view show {view} lists its changes"),
                ));
            }
        }
        // 3. the dependents
        let gone: BTreeSet<Hash> = requested.iter().copied().collect();
        let deps = dependents(&st, &included, &gone)?;
        if !deps.is_empty() && !cascade {
            return Err(dependents_refusal(
                &st,
                &gone,
                &deps,
                E_HAS_DEPENDENTS,
                "what you are excluding",
                "exclude them too with --cascade (MCP: cascade: true)",
                |d| format!("exclude {} too", d.short()),
            )?);
        }
        // 4. the write
        drop(st);
        let all: Vec<Hash> = requested.iter().chain(&deps).copied().collect();
        self.store.view_append(
            view,
            vec![ViewOpKind::Exclude { changes: all }],
            &s.provenance(),
        )?;
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        op_result(&self.read(), view, &[], &[], &requested, &deps)
    }
}

/// The refusal (`code`) of a write that would exclude `gone` while the included changes in
/// `deps` depend on it: `E_HAS_DEPENDENTS` for an exclude, `E_UNDO_BLOCKED` for an undo. The
/// message names `what` the write would exclude; one issue per dependent, with `item_fix`.
pub(crate) fn dependents_refusal(
    st: &ReadState,
    gone: &BTreeSet<Hash>,
    deps: &[Hash],
    code: &'static str,
    what: &str,
    fix: &str,
    item_fix: impl Fn(&Hash) -> String,
) -> Result<RdyError, RdyError> {
    let excluded: BTreeSet<Hash> = gone.iter().chain(deps).copied().collect();
    let reader = st.reader();
    let mut issues = vec![];
    for d in deps {
        let on: Vec<Hash> = reader
            .change(d)?
            .deps
            .into_iter()
            .filter(|x| excluded.contains(x))
            .collect();
        issues.push(Issue {
            severity: Severity::Error,
            code,
            block: None,
            message: format!("{} depends on {}", d.short(), shorts(&on)),
            fix: item_fix(d),
        });
    }
    let mut e = RdyError::new(
        code,
        format!(
            "{} included change(s) depend on {what}: {}",
            deps.len(),
            shorts(deps)
        ),
        fix,
    );
    e.issues = issues;
    Ok(e)
}
