//! `rdy commit -m <message>` (wccm1, D-5T7RQ): records the staged files as one change in the
//! bound view, through a journal written before the request, and writes the changed files back in
//! canonical form. `recover` finishes (or drops) a commit whose acknowledgement was lost.

use super::brief::{for_commit, record_refusal, says_something};
use super::{Open, open};
use crate::brief_text::consequences_text;
use crate::brief_text::plain;
use crate::{Conn, Env, call, hash_short, human_session};
use rdy_daemon::{E_DAEMON_GONE, ErrorReply};
use rdy_engine::wc::files::{apply_writeback, changed_since_staged, restore_unrecorded};
use rdy_engine::wc::layout::BLOBS_DIR;
use rdy_engine::wc::plan::{CommitPlan, UPLOAD_PREFIX, plan_commit};
use rdy_engine::wc::project::blob_name;
use rdy_engine::wc::state::{ConflictKind, Pending, Place, WcState, new_nonce, stage_clear};
use rdy_engine::{
    Diagnostic, E_BAD_INPUT, E_BAD_NAME, E_HAS_CHILDREN, E_IO, E_KEY_CLASH, E_PATH_CLASH,
    E_STALE_BASE, E_UNKNOWN_REF, E_VALIDATION, E_WC_CONFLICT, E_WC_DIRTY, E_WC_INVALID,
    E_WC_LOCKED, E_WC_OCCUPIED, E_WC_STALE, Projection, RdyError, W_WC_UNSTAGED_MOVE,
    editorial_class, now_ms, project_pack,
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
    if !matches!(j.editorial.len(), 0 | 2) {
        // #677: two --editorial flags give four values, which no justification reads
        return Err(RdyError::new(
            E_BAD_INPUT,
            "--editorial is given more than once",
            "pass --editorial once: a class (typo, restructure or clarify) and a reason",
        )
        .into());
    }
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
                "run rdy update: it writes the store's version beside each kept file, to resolve as any conflict"
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
    let mut plan = plan_commit(&place, &state, &pack).map_err(|w| invalid(w.diags))?;
    if plan.ops.is_empty() {
        if plan.committed.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "nothing staged",
                "stage files with rdy add <path>",
            )
            .into());
        }
        // R-P11: the only change is a block moved into a file that is not staged; recording
        // nothing would drop the stage the retry needs
        if let Some(w) = plan.warnings.iter().find(|w| w.code == W_WC_UNSTAGED_MOVE) {
            let mut e = ErrorReply::new(
                E_WC_INVALID,
                format!("nothing to record: {}; the stage is kept", w.message),
                format!("{}, then commit again", w.fix),
            );
            e.extra.insert(
                "diagnostics".into(),
                serde_json::to_value(&plan.warnings).unwrap_or(Value::Null),
            );
            return Err(e);
        }
        // the staged files change no block (an edit R-P4 ignores, a comment): the files go back
        // to their checked-out text, their entries are consumed, the store is not written
        let restored = restore_unrecorded(&place, &state, &plan.committed)?;
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
        if !restored.is_empty() {
            text.push_str(&format!(
                "; {} file(s) put back as checked out: {}",
                restored.len(),
                restored.join(", ")
            ));
        }
        text.push_str(&plan_warnings_text(&plan));
        let out = json!({
            "view": view,
            "change": null,
            "files": restored,
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
    if let Err(e) = upload_figures(&place, &mut conn, &mut plan) {
        conn.close();
        return Err(e);
    }
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
    // the error of a commit the store recorded all the same: reported as a warning
    let (reply, recorded_error) = match reply {
        Ok(v) => (v, None),
        Err(mut e) => match after_commit_error(&e.code) {
            AfterError::KeepJournal => {
                // the store may hold the change: the journal stays for the next command
                e.fix = LOST_FIX.into();
                return Err(e);
            }
            // #675: an error can come after the write (a projection error, a writer panic), so
            // the journal goes only when the daemon says the change is not there
            AfterError::Probe => match probe_nonce(env, &state) {
                Some(v) if v["change"].is_string() => (v, Some(e)),
                Some(v) if v["pending"] != true => {
                    state.pending = None;
                    // a refused briefing is replaced by the fresh one the refusal carries
                    record_refusal(&mut state, &e, &view);
                    state.save(&place)?;
                    return Err(map_error(e, &plan, &state, &view));
                }
                _ => {
                    e.fix = LOST_FIX.into();
                    return Err(e);
                }
            },
        },
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
        // in full when it touches stable content or names anything; else one line
        if plan.stable || says_something(b) {
            text.push_str("briefing (cited by this commit):\n");
            text.push_str(&plain(b));
        } else {
            let token: String = b["base"]["token"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(12)
                .collect();
            text.push_str(&format!("briefing cited (token {token})\n"));
        }
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
    if let Some(e) = &recorded_error {
        text.push_str(&format!(
            "\nwarning {}: {} (the commit was recorded)",
            e.code, e.message
        ));
    }
    text.push_str(&plan_warnings_text(&plan));
    text.push_str(&kept_text(&wb.kept));
    text.push_str(&consequences_text(&reply));
    let mut out = reply;
    if let Some(o) = out.as_object_mut() {
        if let Some(e) = recorded_error {
            let w = json!({"code": e.code, "message": e.message, "fix": e.fix});
            match o.get_mut("warnings").and_then(Value::as_array_mut) {
                Some(ws) => ws.push(w),
                None => {
                    o.insert("warnings".into(), json!([w]));
                }
            }
        }
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

/// What a commit does with the journal after an error reply.
enum AfterError {
    /// The daemon may have recorded the change: ask it with the journal's nonce.
    Probe,
    /// The connection went: the journal stays for the next command's recovery.
    KeepJournal,
}

/// #675: only a lost connection skips the question; any other error can come after the write.
fn after_commit_error(code: &str) -> AfterError {
    if code == E_DAEMON_GONE {
        AfterError::KeepJournal
    } else {
        AfterError::Probe
    }
}

/// How far a `wc_nonce` request searches.
enum NonceScope {
    /// The view's newest changes and canon's newest landing: the probe after an error reply,
    /// which runs on every refused commit and must not stall the daemon's writer (msw11).
    Recent,
    /// Every change up to the daemon's scan limit: `recover`, for a reply that never came.
    Full,
}

/// The `wc_nonce` arguments for the journal's commit `p`.
fn nonce_args(p: &Pending, scope: NonceScope) -> Value {
    let rewrite: Vec<String> = p.rewrite.iter().map(ToString::to_string).collect();
    let mut args = json!({"view": p.view, "nonce": p.nonce, "rewrite": rewrite});
    if matches!(scope, NonceScope::Recent) {
        args["recent"] = json!(true);
    }
    args
}

/// The `wc_nonce` reply for the journal's commit, quietly: `None` when the request fails. It
/// searches the recent changes only: the commit's write, when there was one, is the newest.
fn probe_nonce(env: &Env, st: &WcState) -> Option<Value> {
    let p = st.pending.as_ref()?;
    call(env, "wc_nonce", nonce_args(p, NonceScope::Recent)).ok()
}

/// The figure pass (wcfg1): stores each upload's image as a blob, puts the hash in the ops where
/// the planner left `"$upload:<n>"`, and writes the image to `_blobs/<hash>.<ext>` unless the
/// file there already holds its bytes. A `_blobs` or a blob path that exists as something else
/// (a link included) is `E_WC_OCCUPIED`. Before the journal: a failed commit leaves the stored
/// blobs, which are content addressed and harmless.
fn upload_figures(place: &Place, conn: &mut Conn, plan: &mut CommitPlan) -> Result<(), ErrorReply> {
    for (n, up) in plan.uploads.iter().enumerate() {
        let reply = conn.op(
            "wc_put_blob",
            json!({"path": up.file.display().to_string()}),
        )?;
        let hash = reply["hash"].as_str().ok_or_else(|| {
            ErrorReply::new(
                E_BAD_INPUT,
                "the daemon did not return a blob hash",
                "run rdy serve --stop, then commit again, with the same rdy binary",
            )
        })?;
        let placeholder = format!("{UPLOAD_PREFIX}{n}");
        for op in &mut plan.ops {
            if op["fields"]["blob"].as_str() == Some(placeholder.as_str()) {
                op["fields"]["blob"] = json!(hash);
            }
        }
        let dir = place.root.join(BLOBS_DIR);
        let to = dir.join(blob_name(hash, up.media_type.as_deref()));
        // #642: a link at `_blobs` or at the blob's name would take the write outside the tree
        let occupied = |p: &std::path::Path, what: &str| {
            ErrorReply::from(RdyError::new(
                E_WC_OCCUPIED,
                format!("{} is not a {what}", p.display()),
                "move it aside, then run the command again",
            ))
        };
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
            return Err(occupied(&dir, "directory"));
        }
        let existing = match std::fs::symlink_metadata(&to) {
            Ok(m) if m.is_file() => Some(m.len()),
            Ok(_) => return Err(occupied(&to, "regular file")),
            Err(_) => None,
        };
        let read_io = |e: std::io::Error| {
            ErrorReply::from(RdyError::new(
                E_IO,
                format!("cannot read {}: {e}", up.file.display()),
                "check that the file exists and can be read",
            ))
        };
        let io = |e: std::io::Error| {
            ErrorReply::from(RdyError::new(
                E_IO,
                format!("cannot write {}: {e}", to.display()),
                "check the directory and its permissions",
            ))
        };
        let bytes = std::fs::read(&up.file).map_err(read_io)?;
        // #649: a torn or changed file is rewritten (temp file and rename)
        if existing == Some(bytes.len() as u64) && std::fs::read(&to).is_ok_and(|b| b == bytes) {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(io)?;
        rdy_engine::write_durable(&to, &bytes).map_err(io)?;
    }
    Ok(())
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
    let v = call(env, "wc_nonce", nonce_args(&p, NonceScope::Full)).map_err(|mut e| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msw11_an_error_reply_never_drops_the_journal_without_asking_the_daemon() {
        for code in ["E_INTERNAL", "E_PATH_CLASH", "E_IO", "E_WC_INVALID"] {
            assert!(
                matches!(after_commit_error(code), AfterError::Probe),
                "{code}"
            );
        }
        assert!(matches!(
            after_commit_error("E_DAEMON_GONE"),
            AfterError::KeepJournal
        ));
    }

    #[test]
    fn msw11_the_commit_probe_asks_for_recent_changes_only_and_recover_scans_in_full() {
        let p = Pending {
            nonce: "n1".into(),
            view: "work".into(),
            assigned: vec![],
            rewrite: vec![],
            files: vec![],
            at_ms: 0,
        };
        assert_eq!(
            nonce_args(&p, NonceScope::Recent),
            json!({"view": "work", "nonce": "n1", "rewrite": [], "recent": true})
        );
        assert_eq!(
            nonce_args(&p, NonceScope::Full),
            json!({"view": "work", "nonce": "n1", "rewrite": []})
        );
    }
}
