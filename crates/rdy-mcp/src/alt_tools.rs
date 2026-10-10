//! malt1: `rdy_alternatives` (create, add, remove, list, compare, abandon) and its paragraph of
//! `rdy_schema`. The request itself is run by `rdy_engine::alt::run_alt`, the dispatcher the
//! daemon's `alt` op shares.
use crate::tools::{McpState, budget, check_tokens, session};
use rdy_engine::alt::{AltMemberInput, AltPick, AltRequest, run_alt};
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// One member of a set: a label and the open view that holds the alternative.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AltMemberParams {
    /// A short name for the alternative (x, y, "fixed fee").
    pub label: String,
    /// The open view that holds it.
    pub view: String,
}

/// `rdy_propose`'s `alternatives`: the set, and the proposing view's own member.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AltPickParams {
    /// The set id (as-<12 hex>).
    pub set: String,
    /// The member (label or view) of the view you propose from.
    pub chosen: String,
}

impl From<AltPickParams> for AltPick {
    fn from(p: AltPickParams) -> AltPick {
        AltPick {
            set: p.set,
            chosen: p.chosen,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AltParams {
    /// create, add, remove, list, compare or abandon.
    pub op: String,
    /// create: the set's title (the question the members answer, in a few words).
    pub title: Option<String>,
    /// create: the question block the members answer (id, key or path).
    pub question: Option<String>,
    /// create: two or more members, each a label and an open view.
    pub members: Option<Vec<AltMemberParams>>,
    /// add, remove, compare and abandon: the set id (as-<12 hex>; a unique prefix works).
    pub set: Option<String>,
    /// add: the new member's label.
    pub label: Option<String>,
    /// add: the new member's view.
    pub view: Option<String>,
    /// remove: the member's label (else its view).
    pub member: Option<String>,
    /// abandon: why no member is chosen.
    pub reason: Option<String>,
    /// list: every set, not only the open ones (default false).
    pub all: Option<bool>,
    /// list and compare: cut the answer to about this many tokens.
    pub max_tokens: Option<usize>,
}

/// `rdy_schema`'s paragraph on alternative sets.
pub const ALT_DOC: &str = "rdy_alternatives {op, ...}: create {title, question?, members: [{label, view}]} (two or more open views, each in at most one open set), add {set, label, view}, remove {set, member} (a set keeps at least two members; abandon it instead), list {all?}, compare {set} (each member against canon), abandon {set, reason} (closes every member view). Ids look like as-<12 hex>. Changes written in a member view record the set. A change that a member alone held when it joined the set stays bound to it, whatever views include later. A proposal from a member view passes alternatives: {set, chosen} with its own member; landing it chooses that member and closes the others. Codes: E_ALT_UNCHOSEN: a member view proposed without naming its set and member (pass alternatives); E_ALT_MEMBER: a view in two open sets, a member that is not an open view, a chosen member that is not the proposing view's, a set no longer open, or a change bound to another member or to two open sets (propose from the bound member's view); E_ALT_CHOSEN: a change of a member that lost a chosen set, in a proposal that records no decision superseding or refining the choice (land it with one).";

/// `rdy_alternatives`: builds the request field by field and runs it for the session.
pub fn rdy_alternatives(st: &mut McpState, p: AltParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    let max_tokens = p.max_tokens;
    let req = AltRequest {
        op: p.op,
        title: p.title,
        question: p.question,
        members: p.members.map(|ms| {
            ms.into_iter()
                .map(|m| AltMemberInput {
                    label: m.label,
                    view: m.view,
                })
                .collect()
        }),
        set: p.set,
        label: p.label,
        view: p.view,
        member: p.member,
        reason: p.reason,
        all: p.all,
    };
    let read = matches!(req.op.as_str(), "list" | "compare");
    let v = run_alt(&*st.engine, &s, req)?;
    Ok(if read { budget(v, max_tokens) } else { v })
}
