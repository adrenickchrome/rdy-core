//! The brief op: a briefing over blocks and/or a doc or folder path.
use super::*;
use rdy_engine::brief::Coverage;
use rdy_engine::brief::e_page_alone;
use rdy_engine::code_brief::e_code_path_alone;
use rdy_engine::{E_BAD_INPUT, RdyError, Scope};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BriefArgs {
    ids: Option<Vec<String>>,
    path: Option<String>,
    code_path: Option<String>,
    /// Continue a cut briefing: its `next` cursor. Alone (scope and max_tokens allowed).
    page: Option<String>,
    scope: Option<String>,
    /// A write target, named as `edit_block` names it (a leading `view:` is dropped): this view
    /// when it has a head, else canon.
    view: Option<String>,
    max_tokens: Option<usize>,
}

/// A briefing over blocks and/or a doc or folder path, registered so its token can be cited.
/// The scope is the one given (a read: an unknown view is `E_NOT_FOUND`); else `view` as a write
/// target, named as the write ops name it (a leading `view:` is dropped; that view when it has a
/// head, else canon, as a write there would read it); else the session's default view when it
/// has a head, else canon.
pub(super) fn brief(cx: &OpContext, a: BriefArgs) -> Result<Value, RdyError> {
    check_max_tokens(a.max_tokens)?;
    let st = cx.engine.read();
    let own = cx.session.default_view();
    if a.scope.is_some() && a.view.is_some() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "brief takes scope or view, not both",
            "pass scope to read a scope, or view to brief a write target",
        ));
    }
    let scope = match (a.scope.as_deref(), a.view) {
        (Some(t), _) => st.resolve_read_scope(t, Some(&own))?,
        (None, view) => {
            let target = view_of(cx.session, view);
            if st.view_head(&target).is_some() {
                Scope::View(target)
            } else {
                Scope::Canon
            }
        }
    };
    if let Some(cursor) = &a.page {
        if a.ids.is_some() || a.path.is_some() || a.code_path.is_some() {
            return Err(e_page_alone());
        }
        let b = st.brief_page(&scope, &cx.engine.briefs(), cursor, a.max_tokens)?;
        return Ok(to_json(&b));
    }
    if let Some(cp) = &a.code_path {
        if a.ids.is_some() || a.path.is_some() {
            return Err(e_code_path_alone());
        }
        let b = st.brief_code(&scope, cp, Some(cx.cwd), a.max_tokens)?;
        b.briefing.register(&cx.engine.briefs());
        return Ok(to_json(&b));
    }
    let b = st.brief(
        &scope,
        &Coverage {
            ids: a.ids.unwrap_or_default(),
            path: a.path,
        },
        a.max_tokens,
    )?;
    b.register(&cx.engine.briefs());
    Ok(to_json(&b))
}
