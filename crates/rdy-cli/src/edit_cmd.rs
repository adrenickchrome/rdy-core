use crate::conn::{Env, call, io_error};
use crate::output::{hash_short, keys_lines, text};
use crate::{brief_text, wc};
use clap::Subcommand;
use rdy_daemon::ErrorReply;
use rdy_engine::{E_BAD_INPUT, E_IO, RdyError};
use serde_json::{Value, json};

#[derive(Subcommand)]
pub(crate) enum NewCmd {
    Folder {
        path: String,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
    Doc {
        path: String,
        #[arg(long = "type")]
        doc_type: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
}

#[derive(clap::Args)]
pub(crate) struct EditArgs {
    /// An id or a path such as wallets/transactions.
    id: String,
    /// The view to edit in (default: the session's, `work` for the human).
    #[arg(long)]
    view: Option<String>,
    /// Why the edit is made (an editorial clarify). A tracking field of an accepted or
    /// rejected decision changes only with one.
    #[arg(long)]
    reason: Option<String>,
}

pub(crate) fn new(env: &Env, what: NewCmd) -> Result<(Value, String), ErrorReply> {
    let (kind, path, v) = match what {
        NewCmd::Folder { path, view } => {
            let v = call(env, "new_folder", json!({"path": path, "view": view}))?;
            ("folder", path, v)
        }
        NewCmd::Doc {
            path,
            doc_type,
            title,
            view,
        } => {
            let v = call(
                env,
                "new_doc",
                json!({"path": path, "view": view, "doc_type": doc_type, "title": title}),
            )?;
            ("doc", path, v)
        }
    };
    let prefix = if kind == "doc" { "d-" } else { "fd-" };
    let created: Vec<String> = v["created"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(Value::as_str)
        .map(String::from)
        .collect();
    let last = created
        .iter()
        .rfind(|id| id.starts_with(prefix))
        .cloned()
        .unwrap_or_default();
    let mut t = format!(
        "created {kind} {path} ({last}){}",
        brief_text::consequences_text(&v)
    );
    let mut v = v;
    // the new docs and folders also become files in a working copy bound to this view
    let ids: Vec<String> = created
        .into_iter()
        .filter(|id| id.starts_with("d-") || id.starts_with("fd-"))
        .collect();
    // the write is done: a working copy that cannot take the files is a warning, not a
    // failed `rdy new` (a retry would meet E_ALREADY_EXISTS)
    match wc::new_files::write_new(env, text(&v["view"]), &ids) {
        Ok(wrote) => {
            t.push_str(&wrote.text());
            if !wrote.wrote.is_empty() || !wrote.skipped.is_empty() {
                v["files"] = json!(wrote.wrote);
                v["skipped"] = json!(wrote.skipped);
            }
        }
        Err(e) => {
            t.push_str(&format!(
                "\nwarning: not written: {}: {} ({})",
                e.code, e.message, e.fix
            ));
            v["files_error"] = json!(format!("{}: {}", e.code, e.message));
        }
    }
    Ok((v, t))
}

pub(crate) fn run_edit(env: &Env, a: EditArgs) -> Result<(Value, String), ErrorReply> {
    let EditArgs { id, view, reason } = a;
    let v = edit(env, &id, view.as_deref(), reason.as_deref())?;
    // F4 (FW-PR3b): a rename or a top-level name re-derives keys rdy assigned
    let mut t = format!(
        "change {}{}{}",
        hash_short(text(&v["change"])),
        keys_lines(&v, ""),
        brief_text::consequences_text(&v)
    );
    // msw8 (#874): a person sees the write's warnings, W_BRIEF_STALE among them
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    Ok((v, t))
}

/// `rdy edit`: the block as YAML from the `block` op, its briefing from the `brief` op shown as
/// `#` lines above the YAML, `$EDITOR` on a temp file, then the `edit_block` op citing the
/// briefing's token, the block's `version` as the base and its `conflict`, with `reason` as the
/// edit's editorial reason (a block or conflict changed while the editor was open is
/// `E_STALE_BASE`, a briefing that moved is `E_BRIEF_STALE`, and the edit is kept). Each op runs
/// on a session of its own: the editor may stay open for long, and the daemon may stop meanwhile
/// (`rdy serve --stop` after a rebuild), so the write connects afresh (starting a daemon again if
/// need be); it is sent once and never retried. When the write fails, the edited file is kept and
/// named in the error (`fix`, and `saved_edit` in JSON).
fn edit(
    env: &Env,
    id: &str,
    view: Option<&str>,
    reason: Option<&str>,
) -> Result<Value, ErrorReply> {
    let block = call(env, "block", json!({"id": id, "view": view}))?;
    // cbrf1 (Q6c): brief the block, show the briefing as `#` lines above the YAML and cite its
    // token
    let briefing = call(
        env,
        "brief",
        json!({"ids": [text(&block["id"])], "view": view}),
    )?;
    let tmp = tempfile::Builder::new()
        .prefix("rdy-edit-")
        .suffix(".yaml")
        .tempfile()
        .map_err(|e| io_error(e, "check the temp directory"))?;
    std::fs::write(
        tmp.path(),
        format!("{}{}", brief_text::header(&briefing), text(&block["yaml"])),
    )
    .map_err(|e| io_error(e, "check the temp directory"))?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("$EDITOR \"$1\"")
        .arg("sh")
        .arg(tmp.path())
        .env("EDITOR", &editor)
        .status()
        .map_err(|e| {
            ErrorReply::from(RdyError::new(E_IO, format!("{editor}: {e}"), "set $EDITOR"))
        })?;
    if !status.success() {
        return Err(ErrorReply::new(
            E_BAD_INPUT,
            "editor exited with an error; nothing changed",
            "save and quit the editor normally",
        ));
    }
    let yaml = std::fs::read_to_string(tmp.path()).map_err(|e| io_error(e, "retry"))?;
    let brief = text(&briefing["base"]["token"]);
    call(
        env,
        "edit_block",
        // msw8 (#1064): the version and the conflict the editor read, so a block changed
        // meanwhile is refused
        json!({
            "yaml": yaml,
            "view": view,
            "base": block["version"],
            "conflict": block["conflict"],
            "brief": brief,
            "reason": reason,
        }),
    )
    .map_err(|mut e| {
        if let Ok(path) = tmp.into_temp_path().keep() {
            e.fix = format!(
                "{}; your edit is saved at {}: run rdy edit {id} again and paste it in",
                e.fix,
                path.display()
            );
            e.extra
                .insert("saved_edit".into(), json!(path.to_string_lossy()));
        }
        e
    })
}
