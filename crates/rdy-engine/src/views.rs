//! Views as the engine shows and changes them: create and fork (vcreate1), include with its
//! dependency closure and exclude with its dependents (incl1, excl1), the claim a commit makes
//! on an unowned view, the closed-view rules, the conflict reads, and undo and close (undo1).
use crate::error::*;
use crate::read_state::ReadState;
use crate::write::writable_view;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{BlockId, Hash, Op};
use rdy_schema::{Issue, Severity};
use rdy_store::view::{ResolvedOp, ViewOp, ViewOpKind};
use rdy_store::{Conflict, Owner, StoreError};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

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

fn no_view(view: &str) -> RdyError {
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
/// include into a view with no ref creates it; a closed view takes no commit, include or
/// exclude; an agent acts only in a view it owns (`check_view_access`). Propose keeps its own
/// role and existence checks and takes only the ownership rule (legacy `:` views stay
/// proposable by the human).
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
            if h.closed && matches!(action, Action::Commit | Action::Include | Action::Exclude) =>
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

/// `conflicts` as reads show them: each side with its landing and a one-line summary.
pub(crate) fn conflict_infos(
    st: &ReadState,
    conflicts: &[Conflict],
) -> Result<Vec<ConflictInfo>, RdyError> {
    let reader = st.reader();
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
                                let body = change
                                    .ops
                                    .iter()
                                    .rev()
                                    .find_map(|op| match op {
                                        Op::Create { block } | Op::Update { block, .. }
                                            if block.id == c.block =>
                                        {
                                            Some(block.body.as_str())
                                        }
                                        _ => None,
                                    })
                                    .unwrap_or("");
                                first_chars(body, 60).to_string()
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
            format!("{s:?} is not a change id"),
            "pass the id returned by rdy_write, or one listed by rdy view show",
        )
    })
}

/// Whether `change` landed on canon's chain at or before landing `base` (None: nothing is).
pub(crate) fn in_base(st: &ReadState, base: Option<Hash>, change: &Hash) -> Result<bool, RdyError> {
    let Some(base) = base else { return Ok(false) };
    let Some(l) = st.reader().landing_of(change)? else {
        return Ok(false);
    };
    let mut reached = false;
    for (h, _) in st.landings()? {
        reached |= h == base;
        if reached && h == l {
            return Ok(true);
        }
    }
    Ok(false)
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
fn missing_deps(
    st: &ReadState,
    base: Option<Hash>,
    included: &BTreeSet<Hash>,
    added: &[Hash],
) -> Result<Vec<Hash>, RdyError> {
    let reader = st.reader();
    let added_set: BTreeSet<Hash> = added.iter().copied().collect();
    let mut found: Vec<Hash> = vec![];
    let mut found_set: BTreeSet<Hash> = BTreeSet::new();
    let mut queue: VecDeque<Hash> = added.iter().copied().collect();
    while let Some(c) = queue.pop_front() {
        for d in reader.change(&c)?.deps {
            if included.contains(&d)
                || added_set.contains(&d)
                || found_set.contains(&d)
                || in_base(st, base, &d)?
            {
                continue;
            }
            found_set.insert(d);
            found.push(d);
            queue.push_back(d);
        }
    }
    Ok(found)
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

fn strings(hs: &[Hash]) -> Vec<String> {
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
    /// incl1: includes `changes` (from any view) in `view`, with the deps they need that the view
    /// neither includes nor has in its base. Changes already included are skipped; a landed
    /// change is refused. An include into a view with no ref creates it, owned by the session.
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
        let added_deps = missing_deps(&st, base, &included, &added)?;
        // 4. nothing new: no write
        if added.is_empty() {
            return op_result(&st, view, &[], &[], &[], &[]);
        }
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

/// What `rdy undo` did: the `Undo` op it wrote, the op it undid, and that op's inverse on the
/// view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UndoResult {
    pub view: String,
    /// The new `Undo` op.
    pub op: String,
    /// The op it undid.
    pub undone: String,
    /// The undone op's kind (`commit`, `include`, `exclude`, `close`, `own`, `create`, `fork`,
    /// `undo`).
    pub kind: String,
    pub effect: String,
    /// The changes the undo put back in the view, with the deps they need.
    pub included: Vec<String>,
    /// The changes the undo took out of the view.
    pub excluded: Vec<String>,
    /// The included changes that depended on them, excluded too (`cascade`).
    pub cascaded: Vec<String>,
    pub closed: bool,
    pub reopened: bool,
    /// Whether the reopened view was rebased onto canon's head.
    pub rebased: bool,
    pub conflicts: Vec<ConflictInfo>,
}

/// A view `rdy view close` closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CloseResult {
    pub view: String,
    pub reason: String,
}

/// An op kind in snake case, as undo and `view show` report it.
pub(crate) fn kind_name(k: &ViewOpKind) -> &'static str {
    match k {
        ViewOpKind::Create { .. } => "create",
        ViewOpKind::Fork { .. } => "fork",
        ViewOpKind::Commit { .. } => "commit",
        ViewOpKind::Include { .. } => "include",
        ViewOpKind::Exclude { .. } => "exclude",
        ViewOpKind::Rebase { .. } => "rebase",
        ViewOpKind::Undo { .. } => "undo",
        ViewOpKind::Close { .. } => "close",
        ViewOpKind::Own { .. } => "own",
    }
}

/// The net undone set of a view's op chain (newest first): each undone op mapped to the live
/// undo of it. An op is undone when its newest undo is live, that is, not itself undone. Newest
/// first, every undo of an undo is decided before the undo it targets, so after a redo the
/// redone op is live again. One rule for `undo` and `view show`.
pub(crate) fn net_undone(chain: &[(Hash, ViewOp)]) -> HashMap<Hash, Hash> {
    let mut undone: HashMap<Hash, Hash> = HashMap::new();
    let mut decided: HashSet<Hash> = HashSet::new();
    for (h, op) in chain {
        if let ViewOpKind::Undo { target } = op.op
            && decided.insert(target)
            && !undone.contains_key(h)
        {
            undone.insert(target, *h);
        }
    }
    undone
}

fn undo_landing(message: String) -> RdyError {
    RdyError::new(
        E_UNDO_LANDING,
        message,
        "a landing is undone by a revoking decision (rdy_revoke)",
    )
}

/// The landed rule: an undo never brings back or removes landed work. `Some` refusal when
/// `effect` would re-include a landed change, or would exclude changes that all landed and none
/// of which the view still includes.
fn landed_refusal(
    st: &ReadState,
    short: &str,
    effect: &ResolvedOp,
    included: &BTreeSet<Hash>,
) -> Result<Option<RdyError>, RdyError> {
    let reader = st.reader();
    match effect {
        ResolvedOp::Include(cs) => {
            for c in cs {
                if let Some(l) = reader.landing_of(c)? {
                    return Ok(Some(undo_landing(format!(
                        "op {short} would re-include {}, which landed in {}",
                        c.short(),
                        l.short()
                    ))));
                }
            }
            Ok(None)
        }
        ResolvedOp::Exclude(cs) if !cs.is_empty() => {
            if cs.iter().any(|c| included.contains(c)) {
                return Ok(None);
            }
            let mut first = None;
            for c in cs {
                match reader.landing_of(c)? {
                    Some(l) => {
                        first.get_or_insert(l);
                    }
                    None => return Ok(None),
                }
            }
            Ok(first
                .map(|l| undo_landing(format!("op {short}'s change(s) landed in {}", l.short()))))
        }
        _ => Ok(None),
    }
}

/// Whether `text` could name an op: 7 to 64 hex characters.
fn is_op_prefix(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Rdy {
    /// undo1: undoes op `target` of `view`'s chain (by id or unique prefix), or without a target
    /// the session's newest undoable op not yet undone, by appending an `Undo` with the ops its
    /// inverse needs: the excluded changes' dependents (`cascade`, else `E_UNDO_BLOCKED`), a
    /// re-include's missing deps, or a rebase onto canon when it reopens a view. A rebase, and
    /// an undo that would bring back or remove landed work, is `E_UNDO_LANDING`.
    pub fn undo(
        &mut self,
        s: &Session,
        view: &str,
        target: Option<&str>,
        cascade: bool,
    ) -> Result<UndoResult, RdyError> {
        // 1. the rules
        let st = self.read();
        check_view_op(&st, s, view, Action::Undo)?;
        let head = st.view_head(view).ok_or_else(|| no_view(view))?.clone();
        let reader = st.reader();
        // 2. the chain, newest first; each undone op with the (newest) undo of it
        let mut chain: Vec<(Hash, ViewOp)> = vec![];
        let mut cur = Some(head.head);
        while let Some(h) = cur {
            let op = reader.view_op(&h)?;
            cur = op.prev;
            chain.push((h, op));
        }
        // The net undone set: see `net_undone`.
        let undone = net_undone(&chain);
        let included_list = st.included(view);
        let included: BTreeSet<Hash> = included_list.iter().copied().collect();
        // 3 and 4. the target, its effect, and the rules on it
        let (target, target_op, effect) = match target {
            Some(text) => {
                if !is_op_prefix(text) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("{text:?} is not an op id"),
                        format!("rdy view show {view} lists its ops"),
                    ));
                }
                let prefix = text.to_ascii_lowercase();
                let mut found = chain
                    .iter()
                    .filter(|(h, _)| h.to_string().starts_with(&prefix));
                let (h, op) = match (found.next(), found.next()) {
                    (None, _) => {
                        return Err(RdyError::new(
                            E_NOT_FOUND,
                            format!("no op {text} on view {view:?}'s chain"),
                            format!("rdy view show {view} lists its ops"),
                        ));
                    }
                    (Some(_), Some(_)) => {
                        return Err(RdyError::new(
                            E_BAD_INPUT,
                            format!("{text} is ambiguous on view {view:?}'s chain"),
                            "use more characters",
                        ));
                    }
                    (Some((h, op)), None) => (*h, op.clone()),
                };
                if let ViewOpKind::Rebase { landing, .. } = &op.op {
                    return Err(undo_landing(format!(
                        "op {} is a rebase from landing {}",
                        h.short(),
                        landing.short()
                    )));
                }
                if let Some(u) = undone.get(&h) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("op {} is already undone by {}", h.short(), u.short()),
                        format!(
                            "to redo it, undo that undo: rdy undo --view {view} {}",
                            u.short()
                        ),
                    ));
                }
                let effect = reader.undo_effect(view, &h)?;
                if let Some(e) = landed_refusal(&st, &h.short(), &effect, &included)? {
                    return Err(e);
                }
                (h, op, effect)
            }
            None => {
                let mut pick = None;
                for (h, op) in &chain {
                    if op.actor != s.actor
                        || op.run != s.run
                        || !matches!(
                            op.op,
                            ViewOpKind::Commit { .. }
                                | ViewOpKind::Include { .. }
                                | ViewOpKind::Exclude { .. }
                                | ViewOpKind::Close { .. }
                                | ViewOpKind::Own { .. }
                        )
                        || undone.contains_key(h)
                    {
                        continue;
                    }
                    let effect = reader.undo_effect(view, h)?;
                    if landed_refusal(&st, &h.short(), &effect, &included)?.is_none() {
                        pick = Some((*h, op.clone(), effect));
                        break;
                    }
                }
                pick.ok_or_else(|| {
                    let by = match &s.run {
                        Some(run) => format!("{}/{run}", s.actor),
                        None => s.actor.clone(),
                    };
                    RdyError::new(
                        E_NOTHING_TO_UNDO,
                        format!("no undoable op by {by} on view {view:?}"),
                        format!(
                            "name an op: rdy undo --view {view} <op> (rdy view show lists them)"
                        ),
                    )
                })?
            }
        };
        // 5. the ops: the undo, then what its inverse needs
        let mut ops = vec![ViewOpKind::Undo { target }];
        let mut put_back: Vec<Hash> = vec![];
        let mut taken_out: Vec<Hash> = vec![];
        let mut cascaded: Vec<Hash> = vec![];
        let mut rebased = false;
        match &effect {
            ResolvedOp::Exclude(cs) => {
                taken_out = cs
                    .iter()
                    .filter(|c| included.contains(c))
                    .copied()
                    .collect();
                let gone: BTreeSet<Hash> = taken_out.iter().copied().collect();
                let deps = dependents(&st, &included_list, &gone)?;
                if !deps.is_empty() && !cascade {
                    return Err(dependents_refusal(
                        &st,
                        &gone,
                        &deps,
                        E_UNDO_BLOCKED,
                        "what this undo would exclude",
                        "undo with --cascade (MCP: cascade: true) to exclude them too",
                        |d| format!("undo with --cascade to exclude {} too", d.short()),
                    )?);
                }
                if !deps.is_empty() {
                    ops.push(ViewOpKind::Exclude {
                        changes: deps.clone(),
                    });
                    cascaded = deps;
                }
            }
            ResolvedOp::Include(cs) => {
                let mut seen = BTreeSet::new();
                put_back = cs
                    .iter()
                    .filter(|c| !included.contains(c) && seen.insert(**c))
                    .copied()
                    .collect();
                let deps = missing_deps(&st, st.effective_base(view), &included, &put_back)?;
                if !deps.is_empty() {
                    ops.push(ViewOpKind::Include {
                        changes: deps.clone(),
                    });
                    put_back.extend(deps);
                }
            }
            ResolvedOp::Reopen => {
                if !head.pinned
                    && !included_list.is_empty()
                    && let Some(ch) = st.canon_head()
                    && Some(ch) != head.base
                {
                    let mut dropped = vec![];
                    let mut remaining = vec![];
                    for c in included_list.iter() {
                        if reader.landing_of(c)?.is_some() {
                            dropped.push(*c);
                        } else {
                            remaining.push(*c);
                        }
                    }
                    let rest = remaining
                        .iter()
                        .map(|c| reader.change(c))
                        .collect::<Result<Vec<_>, _>>()?;
                    let merged = reader.merge_onto(&reader.state_of(Some(ch))?, &rest)?;
                    ops.push(ViewOpKind::Rebase {
                        landing: ch,
                        dropped,
                        remaining,
                        conflicts: merged.conflicts,
                    });
                    rebased = true;
                }
            }
            ResolvedOp::Close | ResolvedOp::SetOwner(_) | ResolvedOp::Nothing => {}
        }
        let kind = kind_name(&target_op.op);
        let effect_text = match (&target_op.op, &effect) {
            (ViewOpKind::Undo { target: t }, _) => {
                format!("redid {}", kind_name(&reader.view_op(t)?.op))
            }
            (_, ResolvedOp::Exclude(_)) if !taken_out.is_empty() => {
                format!("excluded {} change(s)", taken_out.len() + cascaded.len())
            }
            (_, ResolvedOp::Include(_)) if !put_back.is_empty() => {
                format!("included {} change(s)", put_back.len())
            }
            (_, ResolvedOp::Close) => "closed".to_string(),
            (_, ResolvedOp::Reopen) => "reopened".to_string(),
            (_, ResolvedOp::SetOwner(_)) => "owner restored".to_string(),
            _ => "no change".to_string(),
        };
        // 6. the write, the publish, the result
        let after_undo = ops.len() - 1;
        drop(st);
        let new_head = self.store.view_append(view, ops, &s.provenance())?;
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        let st2 = self.read();
        let mut op = new_head.head;
        for _ in 0..after_undo {
            op = st2.reader().view_op(&op)?.prev.ok_or_else(|| {
                RdyError::new(
                    E_INTERNAL,
                    format!("view {view:?}: the undo's ops end without a prev"),
                    "run rdy fsck",
                )
            })?;
        }
        let conflicts = conflict_infos(&st2, &st2.conflicts(&Scope::View(view.to_string()))?)?;
        Ok(UndoResult {
            view: view.to_string(),
            op: op.to_string(),
            undone: target.to_string(),
            kind: kind.to_string(),
            effect: effect_text,
            included: strings(&put_back),
            excluded: strings(&taken_out),
            cascaded: strings(&cascaded),
            closed: matches!(effect, ResolvedOp::Close),
            reopened: matches!(effect, ResolvedOp::Reopen),
            rebased,
            conflicts,
        })
    }

    /// Closes `views`, each with `reason` ("closed by hand" when empty). Every name is checked
    /// before any write; each view is then closed in its own write, so a store failure on one
    /// leaves the views before it closed, and the error names the view it failed on.
    pub fn view_close(
        &mut self,
        s: &Session,
        views: &[String],
        reason: &str,
    ) -> Result<Vec<CloseResult>, RdyError> {
        if views.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "name at least one view to close",
                "rdy view close <view>...",
            ));
        }
        let reason = if reason.trim().is_empty() {
            "closed by hand"
        } else {
            reason
        };
        // 1. the rules, for every name, before any write (a name given twice is closed once
        //    the first is, so it is already closed)
        let st = self.read();
        let mut seen = BTreeSet::new();
        for v in views {
            check_view_op(&st, s, v, Action::Close)?;
            let h = st.view_head(v).ok_or_else(|| no_view(v))?;
            if h.closed || !seen.insert(v.as_str()) {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("view {v:?} is already closed"),
                    "nothing to do; reopen it with rdy undo if that was the intent",
                ));
            }
        }
        drop(st);
        // 2. one write and one publish per view
        let mut out = vec![];
        for v in views {
            self.store
                .view_append(
                    v,
                    vec![ViewOpKind::Close {
                        reason: reason.to_string(),
                    }],
                    &s.provenance(),
                )
                .map_err(|e| {
                    let mut e = RdyError::from(e);
                    e.message = format!("closing view {v:?}: {}", e.message);
                    e
                })?;
            self.publish(Delta::View { view: v.clone() })?;
            out.push(CloseResult {
                view: v.clone(),
                reason: reason.to_string(),
            });
        }
        Ok(out)
    }
}

/// The refusal (`code`) of a write that would exclude `gone` while the included changes in
/// `deps` depend on it: `E_HAS_DEPENDENTS` for an exclude, `E_UNDO_BLOCKED` for an undo. The
/// message names `what` the write would exclude; one issue per dependent, with `item_fix`.
fn dependents_refusal(
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

/// An owner as `rdy view own` takes it: `<actor>` or `<actor>/<run>`, split at the first `/`.
fn parse_owner(text: &str) -> Result<Owner, RdyError> {
    let (actor, run) = match text.split_once('/') {
        Some((actor, run)) => (actor, Some(run)),
        None => (text, None),
    };
    if actor.is_empty() || run.is_some_and(str::is_empty) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "owner must be <actor>[/<run>]",
            "for example agent:claude/r1, or agent:claude for a session with no run",
        ));
    }
    Ok(Owner {
        actor: actor.to_string(),
        run: run.map(str::to_string),
    })
}

impl Rdy {
    /// own1: the human reassigns `view` to `owner` (`<actor>[/<run>]`), or leaves it unowned
    /// (`None`), so that the next commit claims it. Works on a closed view and leaves it closed.
    pub fn view_own(
        &mut self,
        s: &Session,
        view: &str,
        owner: Option<&str>,
    ) -> Result<ViewHeadInfo, RdyError> {
        if !s.is_human() {
            return Err(RdyError::new(
                E_PERMISSION,
                "only a human reassigns a view",
                "ask the human, or fork the view",
            ));
        }
        if self.read().view_head(view).is_none() {
            return Err(no_view(view));
        }
        let owner = owner.map(parse_owner).transpose()?;
        self.store
            .view_append(view, vec![ViewOpKind::Own { owner }], &s.provenance())?;
        self.publish(Delta::View {
            view: view.to_string(),
        })?;
        head_info(&self.read(), view)
    }

    /// own1: the human closes every open view idle for more than `days` days at `now_ms`, each
    /// as `view_close` does, with the reason "idle for more than {days} days".
    pub fn view_close_stale(
        &mut self,
        s: &Session,
        days: u32,
        now_ms: i64,
    ) -> Result<Vec<CloseResult>, RdyError> {
        if !s.is_human() {
            return Err(RdyError::new(
                E_PERMISSION,
                "only a human closes views in bulk",
                "run rdy view close --stale as the human",
            ));
        }
        let idle: Vec<String> = self
            .read()
            .view_list(false, days, now_ms)?
            .into_iter()
            .filter(|v| v.idle)
            .map(|v| v.name)
            .collect();
        if idle.is_empty() {
            return Ok(vec![]);
        }
        self.view_close(s, &idle, &format!("idle for more than {days} days"))
    }
}
