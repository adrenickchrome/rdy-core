use crate::conn::{Env, call};
use crate::{brief_text, wc};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub(crate) struct BriefArgs {
    /// A doc file, a folder or a directory of the working copy, or a block id or key. Outside a working copy, a target with a `/` is a path.
    target: Option<String>,
    /// A file in a linked code repo: relative to the repo's top folder, or a path from here.
    #[arg(long = "code")]
    code: Option<String>,
    /// canon, an existing view's name, tag:<version>, landing:<id> or view:<name> (with --code).
    #[arg(long, requires = "code")]
    scope: Option<String>,
    /// The budget for the briefing; the longest lists are cut to fit.
    #[arg(long = "max-tokens")]
    max_tokens: Option<usize>,
}

pub(crate) fn brief(env: &Env, a: BriefArgs) -> Result<(Value, String), ErrorReply> {
    let BriefArgs {
        target,
        code,
        scope,
        max_tokens,
    } = a;
    if let Some(target) = target {
        return wc::brief::run(env, target, max_tokens);
    }
    let v = call(
        env,
        "brief",
        json!({"code_path": code, "scope": scope, "max_tokens": max_tokens}),
    )?;
    let t = format!("{}{}", brief_text::code_text(&v), brief_text::plain(&v));
    Ok((v, t))
}
