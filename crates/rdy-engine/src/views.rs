//! Views as the engine shows and changes them (vcreate1): create and fork, the claim a commit
//! makes on an unowned view, the closed-view rules, and the conflict reads.
use crate::error::*;
use crate::read_state::ReadState;
use crate::write::writable_view;
use crate::{Delta, Rdy, Scope, Session};
use rdy_model::{BlockId, Op};
use rdy_store::{Conflict, Owner, StoreError};
use serde::Serialize;

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
#[allow(dead_code)] // called by include, exclude, close, undo and propose
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
