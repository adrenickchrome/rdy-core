//! rdy_read: one block, a page of entries or a proposal.

use super::*;
use crate::read_page;
use rdy_engine::tool_reads::{self, ReadEntries};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadParams {
    /// A block id, key or path (wallets/transactions); <doc>#<heading-slug> reads one section of a doc; id@version or a ref line [id@version] … reads under that version; P-<id>@<n> reads a proposal version. Required unless page is given.
    pub id: Option<String>,
    /// "canon", a view name (an existing view, or your session's), "tag:<version>", "landing:<id>", "proposal:P-<id>@<n>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// compact (default), markdown or yaml.
    pub format: Option<String>,
    /// Only the blocks whose text matches: a substring (any case), or /regex/. A code block shows only its matching lines; in compact format, a block matched only past its compact line (a prose body, table rows, diagram source) shows that line alone. The result's matched counts the blocks kept.
    pub pattern: Option<String>,
    /// Continue a cut read: its next cursor. Alone (scope and max_tokens allowed).
    pub page: Option<String>,
    pub max_tokens: Option<usize>,
}

fn read_page_of(
    st: &mut McpState,
    q: read_page::ReadQuery,
    read: ReadEntries,
    token: &str,
    from: usize,
    max_tokens: Option<usize>,
) -> Value {
    let out = tool_reads::read_page(&read, token, from, max_tokens);
    // a cut read carries `next` and is registered so that a page call can continue it
    if out.get("next").is_some() {
        st.reads.register(token, q);
    }
    out
}

/// The entries of a read as `q` gives it, on the state the engine publishes now.
fn read_entries(st: &McpState, q: &read_page::ReadQuery) -> Result<ReadEntries, RdyError> {
    let st_ = st.engine.read();
    tool_reads::read_entries(
        &st_,
        &q.id,
        &q.view,
        &q.scope,
        &q.format,
        q.pattern.as_deref(),
    )
}

/// pshow1: a proposal version reads as its own compact text and the structured `proposal`.
fn read_proposal(st: &mut McpState, id: &str, p: &ReadParams) -> Result<Value, RdyError> {
    tool_reads::read_proposal(&st.engine.read(), id, p.format.as_deref(), p.max_tokens)
}

pub fn rdy_read(st: &mut McpState, p: ReadParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    if let Some(cursor) = &p.page {
        if p.id.is_some() || p.format.is_some() || p.pattern.is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "page continues a read: pass it alone",
                "pass only page, with scope or max_tokens if you like",
            ));
        }
        let bad_cursor = || {
            RdyError::new(
                E_BAD_INPUT,
                "page is not a cursor from rdy_read's next",
                "pass the next value of a cut rdy_read unchanged",
            )
        };
        let (token, from) = read_page::parse_cursor(cursor).ok_or_else(bad_cursor)?;
        let q = st.reads.get(&token).ok_or_else(|| {
            RdyError::new(
                E_PAGE_STALE,
                "the read behind this page is unknown (the session restarted, or it was evicted): read again",
                PAGE_STALE_FIX,
            )
        })?;
        // a target that is gone, or a view that closed, is a read that moved
        let read = read_entries(st, &q).map_err(|e| {
            if [E_UNKNOWN_BLOCK, E_NOT_FOUND, E_VIEW_CLOSED].contains(&e.code) {
                RdyError::new(
                    E_PAGE_STALE,
                    "the read moved since its first page: read again",
                    PAGE_STALE_FIX,
                )
            } else {
                e
            }
        })?;
        if read.token(&q) != token {
            return Err(RdyError::new(
                E_PAGE_STALE,
                "the read moved since its first page: read again",
                PAGE_STALE_FIX,
            ));
        }
        // a cursor rdy issued always points inside its read
        if from >= read.entries.len() {
            return Err(bad_cursor());
        }
        return Ok(read_page_of(st, q, read, &token, from, p.max_tokens));
    }
    let Some(id) = p.id.as_deref() else {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "pass id, or page to continue a read",
            "id is a block id, key, path, <doc>#<heading-slug> or P-<id>@<n>",
        ));
    };
    if is_proposal_ref(id) {
        if p.pattern.is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "pattern filters block reads, not a proposal version",
                "omit pattern",
            ));
        }
        return read_proposal(st, id, &p);
    }
    let q = read_page::ReadQuery {
        id: id.to_string(),
        scope: p.scope.clone(),
        view: st.view.clone(),
        format: p.format.clone().unwrap_or_else(|| "compact".into()),
        pattern: p.pattern.clone(),
    };
    let read = read_entries(st, &q)?;
    let token = read.token(&q);
    Ok(read_page_of(st, q, read, &token, 0, p.max_tokens))
}
