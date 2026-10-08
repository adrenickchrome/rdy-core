//! `rdy commit -m <message>` (wccm1, D-5T7RQ): records the staged files as one change in the
//! bound view, through a journal written before the request, and writes the changed files back in
//! canonical form. `recover` finishes (or drops) a commit whose acknowledgement was lost.

use super::brief::{for_commit, record_refusal};
use super::{Open, open};
use crate::brief_text::consequences_text;
use crate::brief_text::plain;
use crate::{Conn, Env, call, hash_short, human_session};
use rdy_daemon::{E_DAEMON_GONE, ErrorReply};
use rdy_engine::wc::files::{apply_writeback, changed_since_staged};
use rdy_engine::wc::plan::{CommitPlan, plan_commit};
use rdy_engine::wc::state::{ConflictKind, Pending, Place, WcState, new_nonce, stage_clear};
use rdy_engine::{
    Diagnostic, E_BAD_INPUT, E_BAD_NAME, E_HAS_CHILDREN, E_KEY_CLASH, E_PATH_CLASH, E_STALE_BASE,
    E_UNKNOWN_REF, E_VALIDATION, E_WC_CONFLICT, E_WC_DIRTY, E_WC_INVALID, E_WC_LOCKED, E_WC_STALE,
    Projection, RdyError, editorial_class, now_ms, project_pack,
};
use rdy_model::BlockId;
use rdy_schema::Severity;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// The fix of a commit whose reply never came.
const LOST_FIX: &str = "run rdy status: it finishes the commit if the store recorded it";

/// The engine refusals a commit reports as problems in the staged files.
const FILE_CODES: &[&str] = &[
    E_VALIDATION,
    E_UNKNOWN_REF,
    "E_BAD_REF",
    E_KEY_CLASH,
    E_PATH_CLASH,
    E_BAD_NAME,
    E_BAD_INPUT,
    "E_CHILD_TYPE",
];

/// How the commit is justified: the decisions it carries out, or an editorial class and reason.
pub(super) struct Justify {
    pub implements: Vec<String>,
    pub editorial: Vec<String>,
}

pub(super) fn run(env: &Env, message: String, j: Justify) -> Result<(Value, String), ErrorReply> {
    if env.no_daemon && !human_session(env) {
        // R-I7: the briefing registry lives in one process, so no recorded token would be known
        // to the next command
        return Err(RdyError::new(
            E_BAD_INPUT,
            "agent commits need the daemon",
            "drop --no-daemon, or use the MCP tools",
        )
        .into());
    }
    let Open {
        place,
        lock,
        mut state,
    } = open(env, true)?;
    if !state.conflicts.is_empty() {
        let paths: Vec<&str> = state.conflicts.iter().map(|c| c.path.as_str()).collect();
        return Err(RdyError::new(
            E_WC_CONFLICT,
            format!(
                "{} conflict(s) are unresolved: {}",
                paths.len(),
                paths.join(", ")
            ),
            if state.conflicts.iter().any(|c| c.kind == ConflictKind::Kept) {
                "copy the files named aside, then run rdy checkout --force (it discards uncommitted edits) and make your edits again"
            } else {
                "edit the doc, delete its .theirs.yaml, run rdy add, then commit"
            },
        )
        .into());
    }
    if let [class, _] = j.editorial.as_slice() {
        // checked before the journal: the daemon's refusal would read as a problem in a file
        editorial_class(class)?;
    }
    let view = state.require_bound()?.view.clone();
    let pack = project_pack(&env.project)?;
    let plan = plan_commit(&place, &state, &pack).map_err(|w| invalid(w.diags))?;
    if plan.ops.is_empty() {
        if plan.committed.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "nothing staged",
                "stage files with rdy add <path>",
            )
            .into());
        }
        // the staged files change no block: their entries are consumed, the store is not written
        for rel in &plan.committed {
            stage_clear(&place, rel)?;
            state.stage.deleted.remove(rel);
        }
        state.save(&place)?;
        drop(lock);
        let mut text = format!(
            "nothing to record in {view}: the staged files change no block; {} stage entr{} cleared",
            plan.committed.len(),
            if plan.committed.len() == 1 {
                "y"
            } else {
                "ies"
            }
        );
        text.push_str(&plan_warnings_text(&plan));
        let out = json!({
            "view": view,
            "change": null,
            "files": [],
            "cleared": plan.committed,
            "warnings": plan.warnings,
        });
        return Ok((out, text));
    }
    // the write-back writes the committed files as staged: a later edit would be lost
    let dirty = changed_since_staged(&place, &state, &plan.committed);
    if !dirty.is_empty() {
        return Err(RdyError::new(
            E_WC_DIRTY,
            format!(
                "{} staged file(s) changed after rdy add: {}",
                dirty.len(),
                dirty.join(", ")
            ),
            "rdy add <path> to commit your later edits too, or restore the staged version",
        )
        .into());
    }
    // the commit's one connection: it briefs the touched blocks (human), then records the change
    let mut conn = Conn::open(env)?;
    let cited = match for_commit(env, &mut conn, &state, &view, &plan) {
        Ok(c) => c,
        Err(e) => {
            conn.close();
            return Err(e);
        }
    };
    let nonce = new_nonce();
    state.pending = Some(Pending {
        nonce: nonce.clone(),
        view: view.clone(),
        assigned: plan.created.iter().cloned().collect(),
        rewrite: plan.rewrite.iter().cloned().collect(),
        files: plan.committed.iter().cloned().collect(),
        at_ms: now_ms(),
    });
    if let Err(e) = state.save(&place) {
        conn.close();
        return Err(e.into());
    }
    let mut args = json!({
        "view": view,
        "ops": plan.ops,
        "nonce": nonce,
        "rewrite": plan.rewrite.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "message": message,
    });
    if !j.implements.is_empty() {
        args["implements"] = json!(j.implements);
    }
    if let [class, reason] = j.editorial.as_slice() {
        args["editorial"] = json!({"class": class, "reason": reason});
    }
    if let Some(token) = &cited.token {
        args["brief"] = json!(token);
    }
    let reply = conn.op("wc_commit", args);
    conn.close();
    let reply = match reply {
        Ok(v) => v,
        Err(mut e) if e.code == E_DAEMON_GONE => {
            // the store may hold the change: the journal stays for the next command
            e.fix = LOST_FIX.into();
            return Err(e);
        }
        Err(e) => {
            state.pending = None;
            // a refused briefing is replaced by the fresh one the refusal carries
            record_refusal(&mut state, &e, &view);
            state.save(&place)?;
            return Err(map_error(e, &plan, &state, &view));
        }
    };
    let proj = projection(&reply, &view)?;
    let wb = apply_writeback(
        &place,
        &mut state,
        &proj,
        &plan.rewrite,
        &plan.removed_files,
        &plan.committed,
    )?;
    state.pending = None;
    state.save(&place)?;
    drop(lock);
    let change = reply["change"].as_str().unwrap_or("");
    let mut text = String::new();
    if let Some(b) = &cited.briefing {
        text.push_str("briefing (cited by this commit):\n");
        text.push_str(&plain(b));
    }
    text.push_str(&format!(
        "committed {} to {view}: {} op(s), {} file(s) rewritten",
        hash_short(change),
        plan.ops.len(),
        wb.written.len()
    ));
    for w in reply["warnings"].as_array().into_iter().flatten() {
        text.push_str(&format!(
            "\nwarning {}: {}",
            w["code"].as_str().unwrap_or(""),
            w["message"].as_str().unwrap_or("")
        ));
    }
    text.push_str(&plan_warnings_text(&plan));
    text.push_str(&kept_text(&wb.kept));
    text.push_str(&consequences_text(&reply));
    let mut out = reply;
    if let Some(o) = out.as_object_mut() {
        for k in ["files", "blocks", "blobs", "missing", "head"] {
            o.remove(k);
        }
        o.insert("view".into(), json!(view));
        o.insert("files".into(), json!(wb.written));
        if let Some(b) = cited.briefing {
            o.insert("briefing".into(), b);
        }
        if !wb.kept.is_empty() {
            o.insert("kept".into(), json!(wb.kept));
        }
    }
    Ok((out, text))
}

/// One line per path the write-back left as it was.
fn kept_text(kept: &[Diagnostic]) -> String {
    kept.iter()
        .map(|d| format!("\nwarning {}: {} (fix: {})", d.code, d.message, d.fix))
        .collect()
}

fn plan_warnings_text(plan: &CommitPlan) -> String {
    plan.warnings
        .iter()
        .map(|d| format!("\nwarning {}: {}", d.code, d.message))
        .collect()
}

/// The projection the keys `files`, `blocks`, `blobs`, `missing` and `head` of a `wc_commit` or
/// `wc_nonce` reply carry.
fn projection(reply: &Value, view: &str) -> Result<Projection, ErrorReply> {
    let mut m = Map::new();
    m.insert("view".into(), json!(view));
    for k in ["files", "blocks", "blobs", "missing", "head"] {
        m.insert(k.into(), reply[k].clone());
    }
    serde_json::from_value(Value::Object(m)).map_err(|e| {
        ErrorReply::new(
            E_BAD_INPUT,
            format!("the daemon's projection does not parse: {e}"),
            "run rdy serve --stop, then rdy status (it finishes the commit), with the same rdy binary",
        )
    })
}

/// Finishes a commit whose reply never came (wccm1, exact), when the journal holds one: asks the
/// store for the change with the journal's nonce. Found: writes the journal's files back, clears
/// the journal and says so on stderr. Not found: clears the journal (the stage stays) and says
/// so. A failed request leaves the journal and fails the command.
pub(super) fn recover(env: &Env, place: &Place, st: &mut WcState) -> Result<(), ErrorReply> {
    let Some(p) = st.pending.clone() else {
        return Ok(());
    };
    let rewrite: Vec<String> = p.rewrite.iter().map(ToString::to_string).collect();
    let v = call(
        env,
        "wc_nonce",
        json!({"view": p.view, "nonce": p.nonce, "rewrite": rewrite}),
    )
    .map_err(|mut e| {
        e.message = format!("cannot finish an interrupted commit: {}", e.message);
        e
    })?;
    if v["pending"] == true {
        // the daemon still runs the commit: the journal stays, and nothing may commit meanwhile
        return Err(RdyError::new(
            E_WC_LOCKED,
            "the interrupted commit is still being recorded by the daemon",
            "run the command again in a moment",
        )
        .into());
    }
    match v["change"].as_str() {
        None => {
            st.pending = None;
            st.save(place)?;
            eprintln!(
                "notice: the interrupted commit did not reach the store; your files are still staged"
            );
        }
        Some(change) => {
            let proj = projection(&v, &p.view)?;
            let rewrite: BTreeSet<BlockId> = p.rewrite.iter().cloned().collect();
            let committed: BTreeSet<String> = p.files.iter().cloned().collect();
            // a staged deletion the change recorded: its file and base copy go
            let removed: BTreeSet<String> = committed
                .iter()
                .filter(|f| st.stage.deleted.contains(*f))
                .cloned()
                .collect();
            let wb = apply_writeback(place, st, &proj, &rewrite, &removed, &committed)?;
            st.pending = None;
            st.save(place)?;
            eprintln!(
                "notice: recovered commit {}: its files are written back",
                hash_short(change)
            );
            for line in kept_text(&wb.kept).lines().filter(|l| !l.is_empty()) {
                eprintln!("{line}");
            }
        }
    }
    Ok(())
}

/// The `E_WC_INVALID` reply for `diags` (all of them under `diagnostics`).
fn invalid(diags: Vec<Diagnostic>) -> ErrorReply {
    let first = diags
        .first()
        .map(|d| {
            format!(
                "{}:{}:{} {} {}",
                d.file, d.line, d.column, d.code, d.message
            )
        })
        .unwrap_or_default();
    let mut e = ErrorReply::new(
        E_WC_INVALID,
        format!("{} problem(s) in staged files; first: {first}", diags.len()),
        "fix the listed files, or make the change through the tool named",
    );
    e.extra.insert(
        "diagnostics".into(),
        serde_json::to_value(&diags).unwrap_or(Value::Null),
    );
    e
}

/// The first block id `text` names.
fn first_id(text: &str) -> Option<BlockId> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find_map(|w| BlockId::parse(w).ok())
}

/// Where a diagnostic about `id` goes: its place in the commit tree, else line 1 of the file the
/// base holds it in.
fn place_of(plan: &CommitPlan, st: &WcState, id: &BlockId) -> Option<(String, usize, usize)> {
    plan.positions
        .get(id)
        .cloned()
        .or_else(|| st.base.blocks.get(id).map(|b| (b.file.clone(), 1, 1)))
}

/// The first committed path, for a problem no block places.
fn first_file(plan: &CommitPlan) -> String {
    plan.committed.iter().next().cloned().unwrap_or_default()
}

/// A refused commit's reply (wccm1's error mapping).
fn map_error(e: ErrorReply, plan: &CommitPlan, st: &WcState, view: &str) -> ErrorReply {
    let code = e.code.as_str();
    if code == E_STALE_BASE {
        let id = first_id(&e.message);
        let file = id
            .as_ref()
            .and_then(|id| place_of(plan, st, id))
            .map_or_else(|| first_file(plan), |p| p.0);
        let shown = id.map_or_else(|| "a block".to_string(), |i| i.to_string());
        return ErrorReply::new(
            E_WC_STALE,
            format!("{shown} ({file}) changed in view {view} since the checkout"),
            "run rdy update first, then commit again",
        );
    }
    if code == E_HAS_CHILDREN
        && let Some(id) = first_id(&e.message)
        && plan
            .ops
            .iter()
            .any(|o| o["op"] == "delete" && o["id"].as_str() == Some(id.as_str()))
    {
        let file = st
            .base
            .blocks
            .get(&id)
            .map_or_else(|| first_file(plan), |b| b.file.clone());
        return invalid(vec![Diagnostic {
            file,
            line: 1,
            column: 1,
            code: E_HAS_CHILDREN.into(),
            message: format!(
                "{id} holds blocks that live only in the store (decisions, questions or other supporting blocks)"
            ),
            fix: "move them with rdy_write, or keep the doc".into(),
            severity: Severity::Error,
        }]);
    }
    if !FILE_CODES.contains(&code) {
        return e;
    }
    let at = |block: Option<BlockId>, message: &str| {
        block
            .or_else(|| first_id(message))
            .and_then(|id| place_of(plan, st, &id))
            .unwrap_or_else(|| (first_file(plan), 1, 1))
    };
    let issues: Vec<&Value> = e
        .extra
        .get("issues")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let diags: Vec<Diagnostic> = if issues.is_empty() {
        let (file, line, column) = at(None, &e.message);
        vec![Diagnostic {
            file,
            line,
            column,
            code: e.code.clone(),
            message: e.message.clone(),
            fix: e.fix.clone(),
            severity: Severity::Error,
        }]
    } else {
        issues
            .into_iter()
            .map(|i| {
                let message = i["message"].as_str().unwrap_or("").to_string();
                let block = i["block"].as_str().and_then(|b| BlockId::parse(b).ok());
                let (file, line, column) = at(block, &message);
                Diagnostic {
                    file,
                    line,
                    column,
                    code: i["code"].as_str().unwrap_or(code).to_string(),
                    message,
                    fix: i["fix"].as_str().unwrap_or("").to_string(),
                    severity: if i["severity"] == "warning" {
                        Severity::Warning
                    } else {
                        Severity::Error
                    },
                }
            })
            .collect()
    };
    invalid(diags)
}
