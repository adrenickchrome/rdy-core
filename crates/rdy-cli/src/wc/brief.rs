//! `rdy brief <file|dir|block>` and recorded briefings (wcbr1, FW-E3, FW-E4, Q6c). In a bound
//! working copy a briefing's token is recorded in `briefs.yaml`; a commit cites a token for the
//! blocks it touches: a human session briefs itself (`for_commit`), an agent session must have
//! recorded one first.

use super::add::under_root;
use super::{Open, open};
use crate::brief_text::plain;
use crate::{Conn, Env, bound_view, call, human_session};
use rdy_daemon::ErrorReply;
use rdy_engine::wc::plan::CommitPlan;
use rdy_engine::wc::state::{BriefEntry, WcState};
use rdy_engine::{E_BAD_INPUT, E_BRIEF_REQUIRED, E_BRIEF_STALE, RdyError, now_ms};
use rdy_model::BlockId;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

const BAD_TARGET: &str = "brief a file, a directory or a block";

/// `rdy brief <target>`: in a bound working copy a file or a directory there, or a block id or
/// key; outside one, a path when the target holds a `/`, else an id or key. In a working copy the
/// briefing's token is recorded.
pub(crate) fn run(
    env: &Env,
    target: String,
    max_tokens: Option<usize>,
) -> Result<(Value, String), ErrorReply> {
    if env.place.is_some() && bound_view(env).is_some() {
        let Open {
            place,
            lock,
            mut state,
        } = open(env, true)?;
        let view = state.require_bound()?.view.clone();
        let mut args = match tree_path(&place.root, &env.cwd, &target)? {
            Some(path) => json!({"path": path}),
            None => json!({"ids": [target]}),
        };
        args["view"] = json!(view);
        args["max_tokens"] = json!(max_tokens);
        let mut briefing = call(env, "brief", args)?;
        let recorded = match entry_of(&briefing, &view) {
            Some(entry) => {
                let n = entry.ids.len();
                keep(&mut state, entry);
                state.save(&place)?;
                Some(n)
            }
            None => None,
        };
        drop(lock);
        return Ok(finish(&mut briefing, recorded));
    }
    let args = if target.contains('/') {
        json!({"path": target, "max_tokens": max_tokens})
    } else {
        json!({"ids": [target], "max_tokens": max_tokens})
    };
    let mut briefing = call(env, "brief", args)?;
    Ok(finish(&mut briefing, None))
}

/// The reply and its text: the briefing with `recorded`, and the recorded count when there is one.
fn finish(briefing: &mut Value, recorded: Option<usize>) -> (Value, String) {
    let mut text = plain(briefing);
    if let Some(n) = recorded {
        text.push_str(&format!("token recorded for {n} block(s)\n"));
    }
    if let Some(o) = briefing.as_object_mut() {
        o.insert("recorded".into(), json!(recorded.is_some()));
    }
    (briefing.clone(), text)
}

/// The tree path `target` names when it is an existing path under `root` (as typed, relative to
/// `cwd`): `x/y.yaml` is `x/y`, `x/_folder.yaml` and the directory `x` are `x`. `None` when it
/// names no path in the working tree (it is a block id or key then); the root itself, and a file
/// that is not a doc or folder file, are refused.
fn tree_path(root: &Path, cwd: &Path, target: &str) -> Result<Option<String>, ErrorReply> {
    let full = cwd.join(target);
    let Ok(meta) = std::fs::symlink_metadata(&full) else {
        return Ok(None);
    };
    let Some(rel) = under_root(root, cwd, Path::new(target)) else {
        return Ok(None);
    };
    let bad = || -> ErrorReply {
        RdyError::new(
            E_BAD_INPUT,
            BAD_TARGET,
            "name a doc file, a folder or a directory below the root, or a block id or key",
        )
        .into()
    };
    if rel.is_empty() {
        return Err(bad());
    }
    let path = if meta.is_dir() {
        rel
    } else if meta.is_file() {
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", &rel));
        let stem = if name == "_folder.yaml" || name == "_folder.theirs.yaml" {
            None
        } else {
            let doc = name
                .strip_suffix(".theirs.yaml")
                .or_else(|| name.strip_suffix(".yaml"))
                .ok_or_else(bad)?;
            Some(doc)
        };
        match (dir, stem) {
            ("", None) => return Err(bad()),
            (dir, None) => dir.to_string(),
            ("", Some(doc)) => doc.to_string(),
            (dir, Some(doc)) => format!("{dir}/{doc}"),
        }
    } else {
        return Err(bad());
    };
    Ok(Some(path))
}

/// The entry a briefing records: its token, the blocks it covers and the view it is for;
/// `None` when it carries no token.
fn entry_of(briefing: &Value, view: &str) -> Option<BriefEntry> {
    let token = briefing["base"]["token"].as_str()?.to_string();
    let ids = briefing["base"]["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| BlockId::parse(b["id"].as_str()?).ok())
        .collect();
    Some(BriefEntry {
        token,
        ids,
        view: view.to_string(),
        at_ms: now_ms(),
    })
}

/// Records `entry` (newest last), dropping an older entry with its token first; the engine
/// keeps the newest `MAX_BRIEFS`.
fn keep(state: &mut WcState, entry: BriefEntry) {
    state.briefs.retain(|b| b.token != entry.token);
    state.record_brief(entry);
}

/// Records the fresh briefing a refused commit carries (`E_BRIEF_REQUIRED`, `E_BRIEF_STALE`), so
/// the next commit cites it. True when something was recorded.
pub(super) fn record_refusal(state: &mut WcState, e: &ErrorReply, view: &str) -> bool {
    if e.code != E_BRIEF_REQUIRED && e.code != E_BRIEF_STALE {
        return false;
    }
    match e.extra.get("briefing").and_then(|b| entry_of(b, view)) {
        Some(entry) => {
            keep(state, entry);
            true
        }
        None => false,
    }
}

/// What a commit cites: the token to send as `brief`, and the briefing to show when the commit
/// briefed itself.
#[derive(Default)]
pub(super) struct Cited {
    pub token: Option<String>,
    pub briefing: Option<Value>,
}

/// The blocks a commit touches: the plan's touched ids that exist in the base, and the parent of
/// every create whose parent the plan does not create.
fn touched(plan: &CommitPlan, state: &WcState) -> BTreeSet<BlockId> {
    let mut t: BTreeSet<BlockId> = plan
        .touched
        .iter()
        .filter(|id| state.base.blocks.contains_key(*id))
        .cloned()
        .collect();
    for op in &plan.ops {
        if op["op"] == "create"
            && let Some(p) = op["parent"].as_str().and_then(|p| BlockId::parse(p).ok())
            && !plan.created.contains(&p)
        {
            t.insert(p);
        }
    }
    t
}

/// The briefing token the commit of `plan` cites (exact; nothing when it touches no existing
/// block). A human session asks for a briefing of the touched blocks on the commit's connection
/// `conn` and cites its token when it covers any block. An agent session cites the token of the
/// newest recorded briefing of `view` that covers every touched block, if any.
pub(super) fn for_commit(
    env: &Env,
    conn: &mut Conn,
    state: &WcState,
    view: &str,
    plan: &CommitPlan,
) -> Result<Cited, ErrorReply> {
    let t = touched(plan, state);
    if t.is_empty() {
        return Ok(Cited::default());
    }
    if human_session(env) {
        let ids: Vec<String> = t.iter().map(ToString::to_string).collect();
        let briefing = conn.op("brief", json!({"ids": ids, "view": view}))?;
        let covers = briefing["base"]["blocks"]
            .as_array()
            .is_some_and(|b| !b.is_empty());
        return Ok(match briefing["base"]["token"].as_str() {
            Some(token) if covers => Cited {
                token: Some(token.to_string()),
                briefing: Some(briefing.clone()),
            },
            _ => Cited::default(),
        });
    }
    let token = state
        .briefs
        .iter()
        .rev()
        .find(|b| b.view == view && t.iter().all(|id| b.ids.contains(id)))
        .map(|b| b.token.clone());
    Ok(Cited {
        token,
        briefing: None,
    })
}
