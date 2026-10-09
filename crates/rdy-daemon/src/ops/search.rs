//! The search op: the blocks whose text matches.
use super::*;
use rdy_engine::search::SearchFilters;
use rdy_engine::{RdyError, SearchQuery};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArgs {
    query: String,
    regex: Option<bool>,
    ignore_case: Option<bool>,
    scope: Option<String>,
    max_tokens: Option<usize>,
    page: Option<String>,
    types: Option<Vec<String>>,
    traits: Option<Vec<String>>,
    path: Option<String>,
    lifecycle: Option<String>,
    changed_since: Option<String>,
    exclude_history: Option<bool>,
    governed_by: Option<String>,
    describes: Option<String>,
    linked_to: Option<String>,
    suspect: Option<bool>,
    tension: Option<bool>,
    mentions: Option<String>,
}

/// msrch1: the blocks whose text matches, in canon unless a scope is named.
pub(super) fn search(cx: &OpContext, a: SearchArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let r = st.search(
        &scope,
        &SearchQuery {
            text: a.query,
            regex: a.regex.unwrap_or(false),
            ignore_case: a.ignore_case.unwrap_or(false),
            filters: SearchFilters {
                types: a.types.unwrap_or_default(),
                traits: a.traits.unwrap_or_default(),
                path: a.path,
                lifecycle: a.lifecycle,
                changed_since: a.changed_since,
                exclude_history: a.exclude_history.unwrap_or(false),
                governed_by: a.governed_by,
                describes: a.describes,
                linked_to: a.linked_to,
                suspect: a.suspect.unwrap_or(false),
                tension: a.tension.unwrap_or(false),
                mentions: a.mentions,
            },
        },
        a.page.as_deref(),
        a.max_tokens,
    )?;
    Ok(to_json(&r))
}
