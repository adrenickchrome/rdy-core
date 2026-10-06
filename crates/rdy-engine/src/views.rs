//! Views as the engine shows and changes them: create and fork (vcreate1), include with its
//! dependency closure and exclude with its dependents (incl1, excl1), the claim a commit makes
//! on an unowned view, the closed-view rules, and the conflict reads.
use crate::error::*;
use crate::read_state::ReadState;
use crate::write::writable_view;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{BlockId, Hash, Op};
use rdy_schema::{Issue, Severity};
use rdy_store::view::ViewOpKind;
use rdy_store::{Conflict, Owner, StoreError};
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};

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

/// The role, existence and closed rules for `action` on `view`. A commit or an include into a
/// view with no ref creates it; a closed view takes no commit, include or exclude. Propose keeps
/// its own checks (legacy `:` views stay proposable).
pub(crate) fn check_view_op(
    st: &ReadState,
    s: &Session,
    view: &str,
    action: Action,
) -> Result<(), RdyError> {
    if action == Action::Propose {
        return Ok(());
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
        None if matches!(action, Action::Commit | Action::Include) => Ok(()),
        None => Err(no_view(view)),
        Some(h)
            if h.closed && matches!(action, Action::Commit | Action::Include | Action::Exclude) =>
        {
            Err(StoreError::ViewClosed(view.to_string()).into())
        }
        Some(_) => Ok(()),
    }
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
        let added_set: BTreeSet<Hash> = added.iter().copied().collect();
        let mut added_deps: Vec<Hash> = vec![];
        let mut dep_set: BTreeSet<Hash> = BTreeSet::new();
        let mut queue: VecDeque<Hash> = added.iter().copied().collect();
        while let Some(c) = queue.pop_front() {
            for d in reader.change(&c)?.deps {
                if included.contains(&d)
                    || added_set.contains(&d)
                    || dep_set.contains(&d)
                    || in_base(&st, base, &d)?
                {
                    continue;
                }
                dep_set.insert(d);
                added_deps.push(d);
                queue.push_back(d);
            }
        }
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
            let excluded: BTreeSet<Hash> = gone.iter().chain(&deps).copied().collect();
            let reader = st.reader();
            let mut issues = vec![];
            for d in &deps {
                let on: Vec<Hash> = reader
                    .change(d)?
                    .deps
                    .into_iter()
                    .filter(|x| excluded.contains(x))
                    .collect();
                issues.push(Issue {
                    severity: Severity::Error,
                    code: E_HAS_DEPENDENTS,
                    block: None,
                    message: format!("{} depends on {}", d.short(), shorts(&on)),
                    fix: format!("exclude {} too", d.short()),
                });
            }
            let mut e = RdyError::new(
                E_HAS_DEPENDENTS,
                format!(
                    "{} included change(s) depend on what you are excluding: {}",
                    deps.len(),
                    shorts(&deps)
                ),
                "exclude them too with --cascade (MCP: cascade: true)",
            );
            e.issues = issues;
            return Err(e);
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
