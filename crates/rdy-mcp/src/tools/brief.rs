//! rdy_brief: the briefing before an edit.

use super::*;
use rdy_engine::brief::e_page_alone;
use rdy_engine::code_brief::e_code_path_alone;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BriefParams {
    /// Block ids, keys or paths; <doc>#<heading-slug> covers that section and every block under it.
    pub ids: Option<Vec<String>>,
    /// A doc or folder path, or <doc>#<heading-slug>: brief on every block under it.
    pub path: Option<String>,
    /// A file in a linked code repo (relative to its top folder): brief on the design blocks its commits implement (Implements: trailers) and the blocks whose reflects globs match it. Not with ids or path.
    pub code_path: Option<String>,
    /// Continue a cut briefing: its next cursor. Alone (scope and max_tokens allowed); the scope must be the first page's.
    pub page: Option<String>,
    /// The view to brief in; defaults to the session's view.
    pub scope: Option<String>,
    /// The budget for the briefing; the longest lists are cut to fit.
    pub max_tokens: Option<usize>,
}

pub fn rdy_brief(st: &mut McpState, p: BriefParams) -> Result<Value, RdyError> {
    let mut out = brief_reply(st, p)?;
    // ain4: every successful briefing route carries the reminder while it is owed, outside the
    // briefing's facts and token
    let s = session(st)?;
    if super::reminder_due(st, &s) {
        super::add_reminder(&mut out);
    }
    Ok(out)
}

/// The briefing of one `rdy_brief` call: a continued page, a code file's, or the blocks' and
/// paths'.
fn brief_reply(st: &mut McpState, p: BriefParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    session(st)?;
    let st_ = st.engine.read();
    let scope = scope_of(&st.view, &st_, &p.scope)?;
    if let Some(cursor) = &p.page {
        if p.ids.is_some() || p.path.is_some() || p.code_path.is_some() {
            return Err(e_page_alone());
        }
        let b = st_.brief_page(&scope, &st.engine.briefs(), cursor, p.max_tokens)?;
        return serialize_briefing(&b);
    }
    if let Some(cp) = &p.code_path {
        if p.ids.is_some() || p.path.is_some() {
            return Err(e_code_path_alone());
        }
        let b = st_.brief_code(&scope, cp, None, p.max_tokens)?;
        b.briefing.register(&st.engine.briefs());
        return serialize_briefing(&b);
    }
    let b = st_.brief(
        &scope,
        &Coverage {
            ids: p.ids.unwrap_or_default(),
            path: p.path,
        },
        p.max_tokens,
    )?;
    b.register(&st.engine.briefs());
    serialize_briefing(&b)
}

pub(crate) use rdy_engine::tool_reads::serialize_briefing;
