//! The read and search replies for a host that embeds rdy core (D-7V6NJ, TASK-1613).
//!
//! [`Snapshot::read`] and [`Snapshot::search`] return the JSON body that `rdy_read` and
//! `rdy_search` return for the same parameters on the same state, because both call one builder
//! (`rdy_engine::tool_reads`). They omit the transport `reminder`. The adapter has no session, so
//! three inputs that belong to one are refused with `E_BAD_INPUT`: a view name as `scope`, `page`
//! and `brief: true`. The scopes it accepts are `canon`, `tag:<version>`, `landing:<id>` and
//! `proposal:P-<id>@<n>`; none is a default but `canon`. A full landing hash (`landing:` and 64
//! lowercase hex) reads any landing that D-Y32MN's reach finds from the snapshot's captured
//! refs: canon, a tag, a held chain or a replaced mark, and a met landing's `rebased_from`. A
//! shorter landing id matches among canon's chain, as `rdy_read` matches it. In a holder,
//! `view:<name>@<op>` (the op's 64 lowercase hex digits) reads a pushed view at that op of the
//! chain of its captured head, with the base and the included set at that op (D-00ZG2); a
//! project keeps refusing every view scope, because its views have owners and sessions. A cut
//! reply carries `truncated` and `omitted` and no `next`: a host narrows the read instead of
//! paging it.

use crate::store::Snapshot;
use rdy_engine::search::{SearchFilters, SearchQuery};
use rdy_engine::tool_reads::{self, check_tokens};
use rdy_engine::{E_BAD_INPUT, E_NOT_FOUND, RdyError, ReadState, Scope, is_proposal_ref};
use rdy_model::Hash;
use rdy_store::StoreError;
use rdy_store::holder::StoreMode;
use serde_json::{Value, json};

/// The scope `rdy_read` and `rdy_search` take when none is given.
const DEFAULT_SCOPE: &str = "canon";

/// What `rdy_read` takes, less the session's `page`, which is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadRequest {
    /// A block id, key or path; `<doc>#<heading-slug>`; `id@version`; or `P-<id>@<n>` for a
    /// proposal version.
    pub id: String,
    /// `canon` (the default), `tag:<version>`, `landing:<id>` or `proposal:P-<id>@<n>`; in a
    /// holder also `view:<name>@<op>`.
    pub scope: Option<String>,
    /// `compact` (the default), `markdown` or `yaml`.
    pub format: Option<String>,
    /// Keep only the blocks whose text matches: a substring, or `/regex/`.
    pub pattern: Option<String>,
    pub max_tokens: Option<usize>,
    /// Refused: a cursor belongs to a session.
    pub page: Option<String>,
}

/// What `rdy_search` takes, less the session's `page` and `brief`, which are refused.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    pub regex: bool,
    pub ignore_case: bool,
    /// `canon` (the default), `tag:<version>`, `landing:<id>` or `proposal:P-<id>@<n>`; in a
    /// holder also `view:<name>@<op>`.
    pub scope: Option<String>,
    pub max_tokens: Option<usize>,
    pub filters: SearchFilters,
    /// Refused: a cursor belongs to a session.
    pub page: Option<String>,
    /// Refused: a briefing token belongs to a session.
    pub brief: bool,
}

fn refuse(what: String, fix: &str) -> RdyError {
    RdyError::new(E_BAD_INPUT, what, fix)
}

/// The scope text a host may name; a view name, `view:<name>` and `inflight` belong to a session.
/// A holder's pushed view at an op (`view:<name>@<op>`) belongs to none: `holder` says whether
/// the store is one.
fn check_scope(
    scope: &Option<String>,
    holder: impl FnOnce() -> Result<bool, RdyError>,
) -> Result<(), RdyError> {
    let Some(text) = scope else {
        return Ok(());
    };
    let ok = text == DEFAULT_SCOPE
        || ["tag:", "landing:", "proposal:"]
            .iter()
            .any(|p| text.starts_with(p))
        || (view_op(scope).is_some() && holder()?);
    if ok {
        return Ok(());
    }
    Err(refuse(
        format!("scope {text:?} names a view: a view belongs to a session"),
        "use canon, tag:<version>, landing:<id> or proposal:P-<id>@<n>",
    ))
}

/// The landing a `landing:<64 lowercase hex>` scope names; None for every other scope text.
fn full_landing(scope: &Option<String>) -> Option<Hash> {
    // a hash parses from exactly 64 lowercase hex digits
    scope.as_deref()?.strip_prefix("landing:")?.parse().ok()
}

/// The view and the op a `view:<name>@<64 lowercase hex>` scope names; None for every other
/// scope text.
fn view_op(scope: &Option<String>) -> Option<(&str, Hash)> {
    let (name, op) = scope.as_deref()?.strip_prefix("view:")?.rsplit_once('@')?;
    // a hash parses from exactly 64 lowercase hex digits
    Some((name, op.parse().ok()?))
}

fn check_page(page: &Option<String>) -> Result<(), RdyError> {
    if page.is_some() {
        return Err(refuse(
            "page continues a read of a session: the adapter has none".into(),
            "omit page, and narrow the call with pattern, #section, filters or max_tokens",
        ));
    }
    Ok(())
}

/// A reply without its cursor: the adapter keeps no read to continue.
fn without_next(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("next");
    }
    v
}

/// `E_NOT_FOUND`: a full landing hash that D-Y32MN's reach does not find from the captured refs.
pub(crate) fn unreached(hash: Hash) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("landing {hash} is not reached from canon, a tag, a held chain or a replaced mark"),
        "name a landing this store reaches, or read canon",
    )
}

/// `E_NOT_FOUND`: an op that the chain of view `name`'s captured head does not hold.
fn no_view_op(name: &str, op: Hash) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("view {name:?} has no op {op} in its chain"),
        "name a head op of this view; read its heads with view_heads",
    )
}

/// What a read of a scope text reads: the engine's resolution of the text, a scope the snapshot
/// resolved, or a pushed view's own state at an op, read as that view.
enum Target {
    Engine,
    At(Scope),
    View(Box<ReadState>, Scope),
}

impl Snapshot {
    /// Whether the snapshot's store is a holder.
    fn is_holder(&self) -> Result<bool, RdyError> {
        Ok(self.read_state().reader().mode()? == StoreMode::Holder)
    }

    /// The state of pushed view `name` at `op` (D-00ZG2): `op` must lie in the chain of the
    /// view's captured head, so a later push is never reached; else `E_NOT_FOUND`.
    fn view_state(&self, name: &str, op: Hash) -> Result<ReadState, RdyError> {
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let Some(captured) = self.captured_views().get(name) else {
            return Err(no_view_op(name, op));
        };
        let head = match read.reader().view_head_at(name, &captured.head, &op) {
            Ok(head) => head,
            Err(StoreError::NotFound(x)) if x == op => return Err(no_view_op(name, op)),
            Err(e) => return Err(e.into()),
        };
        let state = read.pushed_view_state(name, head)?;
        read.ensure_current_epoch()?;
        Ok(state)
    }

    /// What a read of `scope` reads (see [`Target`]). The scope text has passed `check_scope`.
    fn target(&self, scope: &Option<String>) -> Result<Target, RdyError> {
        if let Some(s) = self.reached_scope(scope)? {
            return Ok(Target::At(s));
        }
        if let Some((name, op)) = view_op(scope) {
            let state = self.view_state(name, op)?;
            return Ok(Target::View(Box::new(state), Scope::View(name.to_string())));
        }
        Ok(Target::Engine)
    }

    /// The scope of a full landing hash: `Scope::At` the landing when D-Y32MN's reach finds it
    /// from the captured refs, else `E_NOT_FOUND`. None for every other scope text, which the
    /// engine resolves as it does for `rdy_read`.
    fn reached_scope(&self, scope: &Option<String>) -> Result<Option<Scope>, RdyError> {
        let Some(hash) = full_landing(scope) else {
            return Ok(None);
        };
        if self.reached_landing(hash)? {
            return Ok(Some(Scope::At(hash)));
        }
        Err(unreached(hash))
    }

    /// The reply `rdy_read` gives for `req` on this snapshot, without `reminder` and `next`.
    pub fn read(&self, req: ReadRequest) -> Result<Value, RdyError> {
        check_tokens(req.max_tokens)?;
        check_page(&req.page)?;
        check_scope(&req.scope, || self.is_holder())?;
        let state = self.read_state();
        if is_proposal_ref(&req.id) {
            if req.pattern.is_some() {
                return Err(refuse(
                    "pattern filters block reads, not a proposal version".into(),
                    "omit pattern",
                ));
            }
            return tool_reads::read_proposal(
                state,
                &req.id,
                req.format.as_deref(),
                req.max_tokens,
            );
        }
        let format = req.format.clone().unwrap_or_else(|| "compact".into());
        let read = match self.target(&req.scope)? {
            Target::At(scope) => tool_reads::read_entries_at(
                state,
                &req.id,
                &scope,
                &format,
                req.pattern.as_deref(),
            )?,
            Target::View(view, scope) => {
                let read = tool_reads::read_entries_at(
                    &view,
                    &req.id,
                    &scope,
                    &format,
                    req.pattern.as_deref(),
                )?;
                state.ensure_current_epoch()?;
                read
            }
            Target::Engine => tool_reads::read_entries(
                state,
                &req.id,
                DEFAULT_SCOPE,
                &req.scope,
                &format,
                req.pattern.as_deref(),
            )?,
        };
        // the token only names a cursor, and the adapter returns none; its length is the one a
        // cursor has, so the page is cut where `rdy_read` cuts it
        let token = read.token(&json!({"id": req.id, "scope": req.scope, "format": format}));
        Ok(without_next(tool_reads::read_page(
            &read,
            &token,
            0,
            req.max_tokens,
        )))
    }

    /// The reply `rdy_search` gives for `req` on this snapshot, without `reminder` and `next`.
    pub fn search(&self, req: SearchRequest) -> Result<Value, RdyError> {
        check_tokens(req.max_tokens)?;
        check_page(&req.page)?;
        if req.brief {
            return Err(refuse(
                "brief registers a briefing token with a session: the adapter has none".into(),
                "omit brief",
            ));
        }
        check_scope(&req.scope, || self.is_holder())?;
        let state = self.read_state();
        let (view, scope): (Option<Box<ReadState>>, Scope) = match self.target(&req.scope)? {
            Target::At(scope) => (None, scope),
            Target::View(view, scope) => (Some(view), scope),
            Target::Engine => (
                None,
                tool_reads::scope_of(DEFAULT_SCOPE, state, &req.scope)?,
            ),
        };
        let reply = tool_reads::search_reply(
            view.as_deref().unwrap_or(state),
            &scope,
            &SearchQuery {
                text: req.query,
                regex: req.regex,
                ignore_case: req.ignore_case,
                filters: req.filters,
            },
            None,
            req.max_tokens,
            None,
        )?;
        if view.is_some() {
            state.ensure_current_epoch()?;
        }
        Ok(without_next(reply))
    }
}
