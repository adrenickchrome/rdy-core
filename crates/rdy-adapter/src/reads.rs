//! The read and search replies for a host that embeds rdy core (D-7V6NJ, TASK-1613).
//!
//! [`Snapshot::read`] and [`Snapshot::search`] return the JSON body that `rdy_read` and
//! `rdy_search` return for the same parameters on the same state, because both call one builder
//! (`rdy_engine::tool_reads`). They omit the transport `reminder`. The adapter has no session, so
//! three inputs that belong to one are refused with `E_BAD_INPUT`: a view name as `scope`, `page`
//! and `brief: true`. The scopes it accepts are `canon`, `tag:<version>`, `landing:<id>` and
//! `proposal:P-<id>@<n>`; none is a default but `canon`. A cut reply carries `truncated` and
//! `omitted` and no `next`: a host narrows the read instead of paging it.

use crate::store::Snapshot;
use rdy_engine::search::{SearchFilters, SearchQuery};
use rdy_engine::tool_reads::{self, check_tokens};
use rdy_engine::{E_BAD_INPUT, RdyError, Scope, is_proposal_ref};
use serde_json::{Value, json};

/// The scope `rdy_read` and `rdy_search` take when none is given.
const DEFAULT_SCOPE: &str = "canon";

/// What `rdy_read` takes, less the session's `page`, which is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadRequest {
    /// A block id, key or path; `<doc>#<heading-slug>`; `id@version`; or `P-<id>@<n>` for a
    /// proposal version.
    pub id: String,
    /// `canon` (the default), `tag:<version>`, `landing:<id>` or `proposal:P-<id>@<n>`.
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
    /// `canon` (the default), `tag:<version>`, `landing:<id>` or `proposal:P-<id>@<n>`.
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
fn check_scope(scope: &Option<String>) -> Result<(), RdyError> {
    let Some(text) = scope else {
        return Ok(());
    };
    let ok = text == DEFAULT_SCOPE
        || ["tag:", "landing:", "proposal:"]
            .iter()
            .any(|p| text.starts_with(p));
    if ok {
        return Ok(());
    }
    Err(refuse(
        format!("scope {text:?} names a view: a view belongs to a session"),
        "use canon, tag:<version>, landing:<id> or proposal:P-<id>@<n>",
    ))
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

impl Snapshot {
    /// The reply `rdy_read` gives for `req` on this snapshot, without `reminder` and `next`.
    pub fn read(&self, req: ReadRequest) -> Result<Value, RdyError> {
        check_tokens(req.max_tokens)?;
        check_page(&req.page)?;
        check_scope(&req.scope)?;
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
        let read = tool_reads::read_entries(
            state,
            &req.id,
            DEFAULT_SCOPE,
            &req.scope,
            &format,
            req.pattern.as_deref(),
        )?;
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
        check_scope(&req.scope)?;
        let state = self.read_state();
        let scope: Scope = tool_reads::scope_of(DEFAULT_SCOPE, state, &req.scope)?;
        let reply = tool_reads::search_reply(
            state,
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
        Ok(without_next(reply))
    }
}
