//! The search op: the blocks whose text matches.
use super::*;
use rdy_engine::search::SearchFilters;
use rdy_engine::{RdyError, SearchInput};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArgs {
    query: Option<String>,
    like: Option<String>,
    like_text: Option<String>,
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
    coverage_below: Option<u32>,
    sort: Option<String>,
}

/// msrch1: the blocks whose text matches, in canon unless a scope is named; vxs7 (D-76Y9H):
/// or the blocks most like a block (`like`) or a text (`like_text`).
pub(super) fn search(cx: &OpContext, a: SearchArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    // vxs7: the source options become one validated input, once
    let input = SearchInput::new(
        a.query,
        a.like,
        a.like_text,
        a.regex.unwrap_or(false),
        a.ignore_case.unwrap_or(false),
        SearchFilters {
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
            coverage_below: a.coverage_below,
            sort: a.sort,
        },
    )?;
    let st = cx.engine.read();
    let scope = scope_of(&st, cx, a.scope.as_deref())?;
    let r = st.search_input(&scope, &input, a.page.as_deref(), a.max_tokens)?;
    Ok(to_json(&r))
}
