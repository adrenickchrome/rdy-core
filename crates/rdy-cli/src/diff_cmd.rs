//! `rdy diff` and `rdy q`: the chat paths are rdy_diff and rdy_query (D-HZYK0).
use crate::conn::{Env, call};
use crate::diff_text::{diff_text, query_text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};
use std::sync::OnceLock;

#[derive(clap::Args)]
pub(crate) struct DiffArgs {
    /// The older scope: canon, an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n>, view:<name> or inflight. Alone, a view, canon or a proposal version is the newer side. Alone, a tag or a landing is the older side, and canon is the newer.
    from: String,
    /// The newer scope, in the same forms.
    to: Option<String>,
    /// Limit both sides to one doc or folder: an id, a key or a path.
    #[arg(long)]
    doc: Option<String>,
    /// Add field values and the word diff of each changed body.
    #[arg(long)]
    detail: bool,
}

#[derive(clap::Args)]
pub(crate) struct QArgs {
    /// The filter expression (see below).
    filter: String,
    /// canon (the default), an existing view's name, tag:<version>, landing:<id>, proposal:P-<id>@<n>, view:<name> or inflight.
    #[arg(long)]
    scope: Option<String>,
}

/// `rdy q --help`: the one-line help, then the filter guide (O17).
pub(crate) fn q_long_about() -> &'static str {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        format!(
            "Filter blocks with an expression, such as type = decision and status = live. The chat path is rdy_query.\n\n{}",
            rdy_schema::filter::FILTER_GUIDE
        )
    })
}

pub(crate) fn diff(env: &Env, a: DiffArgs) -> Result<(Value, String), ErrorReply> {
    let DiffArgs {
        from,
        to,
        doc,
        detail,
    } = a;
    let v = call(
        env,
        "diff",
        json!({"from": from, "to": to, "doc": doc, "detail": detail}),
    )?;
    let t = diff_text(&v, detail);
    Ok((v, t))
}

pub(crate) fn q(env: &Env, a: QArgs) -> Result<(Value, String), ErrorReply> {
    let QArgs { filter, scope } = a;
    let v = call(env, "query", json!({"filter": filter, "scope": scope}))?;
    let t = query_text(&v);
    Ok((v, t))
}
