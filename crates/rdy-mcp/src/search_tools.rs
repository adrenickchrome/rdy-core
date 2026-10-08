//! msrch1: `rdy_search`, the find-text tool. The search itself is `ReadState::search`.
use crate::tools::{McpState, check_tokens, scope_of, serialize_briefing, session};
use rdy_engine::search::{DEFAULT_SEARCH_TOKENS, SearchFilters};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// The text to find; with regex: true, a pattern (Rust regex syntax, at most 1024 bytes).
    pub query: String,
    /// Treat query as a regular expression (default false: literal text).
    pub regex: Option<bool>,
    /// Match regardless of case (default false).
    pub ignore_case: Option<bool>,
    /// "canon", a view name, "tag:<version>", "landing:<id>" or "view:<name>". Defaults to the session view.
    pub scope: Option<String>,
    /// The budget for this page of matches (default 4000).
    pub max_tokens: Option<usize>,
    /// Continue a cut search: its next cursor, with the same query, flags and scope.
    pub page: Option<String>,
    /// Keep these block types (any of them), e.g. ["code"].
    pub types: Option<Vec<String>>,
    /// Keep blocks whose type has one of these traits: normative, informative, defines, describes, asset, structural.
    pub traits: Option<Vec<String>>,
    /// Keep blocks under this doc or folder path (whole segments), e.g. "guides".
    pub path: Option<String>,
    /// Keep blocks whose lifecycle, as inherited, is draft, stable or deprecated.
    pub lifecycle: Option<String>,
    /// Keep blocks whose version changed since a version (e.g. 1.2.0), tag:<version> or landing:<id>.
    pub changed_since: Option<String>,
    /// Leave out matches inside a superseded, rejected or revoked decision (they carry history: true).
    pub exclude_history: Option<bool>,
    /// Keep blocks this decision governs: through decided_by, implements, or the decision's affects.
    pub governed_by: Option<String>,
    /// Keep blocks with a describes link to this block.
    pub describes: Option<String>,
    /// Keep blocks with any link or reference to or from this block.
    pub linked_to: Option<String>,
    /// Keep only blocks with an open suspect.
    pub suspect: Option<bool>,
    /// Keep only blocks involved in an open tension.
    pub tension: Option<bool>,
    /// Keep blocks that name an entity by name or alias: linked (tied to it) or unlinked (not linked yet).
    pub mentions: Option<String>,
    /// Add a briefing over this page's hits; cite its base.token to write those blocks.
    pub brief: Option<bool>,
}

pub fn rdy_search(st: &mut McpState, p: SearchParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    let brief = p.brief.unwrap_or(false);
    // With a briefing the caller's budget covers both halves: the hits and the briefing.
    let total = p.max_tokens.unwrap_or(DEFAULT_SEARCH_TOKENS);
    let (search_tokens, briefing_tokens) = if brief {
        (Some(total / 2), Some(total - total / 2))
    } else {
        (p.max_tokens, None)
    };
    let r = st_.search(
        &scope,
        &SearchQuery {
            text: p.query,
            regex: p.regex.unwrap_or(false),
            ignore_case: p.ignore_case.unwrap_or(false),
            filters: SearchFilters {
                types: p.types.unwrap_or_default(),
                traits: p.traits.unwrap_or_default(),
                path: p.path,
                lifecycle: p.lifecycle,
                changed_since: p.changed_since,
                exclude_history: p.exclude_history.unwrap_or(false),
                governed_by: p.governed_by,
                describes: p.describes,
                linked_to: p.linked_to,
                suspect: p.suspect.unwrap_or(false),
                tension: p.tension.unwrap_or(false),
                mentions: p.mentions,
            },
        },
        p.page.as_deref(),
        search_tokens,
    )?;
    let briefing = if brief && !r.matches.is_empty() {
        let mut ids: Vec<String> = Vec::new();
        for m in &r.matches {
            for id in std::iter::once(&m.id).chain(m.also.iter().map(|a| &a.id)) {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        // A briefing that fails fails the call: never a token without its briefing.
        let b = st_.brief(&scope, &Coverage { ids, path: None }, briefing_tokens)?;
        b.register(&st.engine.briefs());
        Some(serialize_briefing(&b)?)
    } else {
        None
    };
    let mut v = serde_json::to_value(&r).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a search result did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    if let (Some(b), Some(o)) = (briefing, v.as_object_mut()) {
        o.insert("briefing".to_string(), b);
    }
    Ok(v)
}
