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
    /// A publication profile: the docs and folders a publication holds, and its audience.
    /// `rdy edit` changes it later; ordinary propose, approve and tag land and pin it.
    Profile {
        /// The profile's key (an ordinary block key).
        key: String,
        /// The id of the doc that holds the profile.
        #[arg(long)]
        parent: String,
        #[arg(long)]
        title: String,
        /// Who the publication is for: `org` or `public`.
        #[arg(long)]
        audience: String,
        /// The id of a doc or folder the publication holds; repeat it for more.
        #[arg(long = "select", required = true)]
        select: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        /// Whether the publication carries its decision trail (default: on for an org
        /// audience, off for public).
        #[arg(long)]
        trail: Option<bool>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
    /// A finding: what surfaced, with its evidence. Each --question and --session belongs to
    /// the latest --quote before it; each --source (an id, key or path) is an entry of its own.
    /// It lives only in the store, in the decision home unless --parent names a place. The chat
    /// path is rdy_write.
    Finding {
        #[arg(long)]
        statement: String,
        /// Words quoted as evidence; repeat it for more entries.
        #[arg(long)]
        quote: Vec<String>,
        /// The question the latest --quote answers.
        #[arg(long)]
        question: Vec<String>,
        /// The session the latest --quote comes from.
        #[arg(long)]
        session: Vec<String>,
        /// A source the finding cites: its id, key or path; repeat it for more.
        #[arg(long)]
        source: Vec<String>,
        /// The doc or section to record it in (default: the decision home).
        #[arg(long)]
        parent: Option<String>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
        /// Where each evidence option stood on the command line (`main` fills it).
        #[arg(skip)]
        at: EvidenceAt,
    },
    /// A source: a release, commit or spec that a finding cites. It lives only in the store, in
    /// the decision home unless --parent names a place. The chat path is rdy_write.
    Source {
        #[arg(long)]
        title: String,
        /// release, commit or spec.
        #[arg(long)]
        kind: String,
        /// Where it is: a tag, a commit hash, a URL or a path.
        #[arg(long)]
        locator: String,
        #[arg(long)]
        version: Option<String>,
        /// The doc or section to record it in (default: the decision home).
        #[arg(long)]
        parent: Option<String>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
}

/// The argument indices of `rdy new finding`'s evidence options, in clap's numbering: their
/// order on the command line, which the derived lists lose.
#[derive(Clone, Debug, Default)]
pub(crate) struct EvidenceAt {
    quote: Vec<usize>,
    question: Vec<usize>,
    session: Vec<usize>,
    source: Vec<usize>,
}

impl EvidenceAt {
    /// The indices of the `finding` subcommand's matches.
    pub(crate) fn of(m: &clap::ArgMatches) -> EvidenceAt {
        let at =
            |id: &str| -> Vec<usize> { m.indices_of(id).map(|i| i.collect()).unwrap_or_default() };
        EvidenceAt {
            quote: at("quote"),
            question: at("question"),
            session: at("session"),
            source: at("source"),
        }
    }
}

/// Fills the evidence indices of `rdy new finding` from the parsed command line.
pub(crate) fn fill_evidence_at(what: &mut NewCmd, matches: &clap::ArgMatches) {
    if let NewCmd::Finding { at, .. } = what
        && let Some(m) = matches
            .subcommand_matches("new")
            .and_then(|m| m.subcommand_matches("finding"))
    {
        *at = EvidenceAt::of(m);
    }
}

/// One evidence option of `rdy new finding`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flag {
    Quote,
    Question,
    Session,
    Source,
}

/// The evidence entries of `rdy new finding`, in command-line order: a --quote starts an entry
/// and each --question and --session joins the latest one; a --source is an entry of its own
/// and leaves the latest quote's entry open. `E_BAD_INPUT` for a --question or --session with no
/// --quote before it, or a second one for one quote.
fn evidence_entries(
    quote: Vec<String>,
    question: Vec<String>,
    session: Vec<String>,
    source: Vec<String>,
    at: &EvidenceAt,
) -> Result<Vec<Value>, ErrorReply> {
    let mut events: Vec<(usize, Flag, String)> = vec![];
    for (flag, values, idx) in [
        (Flag::Quote, quote, &at.quote),
        (Flag::Question, question, &at.question),
        (Flag::Session, session, &at.session),
        (Flag::Source, source, &at.source),
    ] {
        for (i, v) in values.into_iter().enumerate() {
            events.push((idx.get(i).copied().unwrap_or(i), flag, v));
        }
    }
    events.sort_by_key(|(i, _, _)| *i);
    let mut out: Vec<Value> = vec![];
    let mut open: Option<usize> = None;
    for (_, flag, v) in events {
        let (name, key) = match flag {
            Flag::Quote => {
                out.push(json!({"quote": v}));
                open = Some(out.len() - 1);
                continue;
            }
            Flag::Source => {
                out.push(json!({"source": v}));
                continue;
            }
            Flag::Question => ("question", "question"),
            Flag::Session => ("session", "session_ref"),
        };
        let Some(entry) = open.map(|i| &mut out[i]) else {
            return Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("--{name} needs a preceding --quote"),
                format!("pass --quote before --{name}"),
            ));
        };
        if entry.get(key).is_some() {
            return Err(ErrorReply::new(
                E_BAD_INPUT,
                format!("--{name} is repeated for one --quote"),
                "give each quote at most one --question and one --session",
            ));
        }
        entry[key] = json!(v);
    }
    Ok(out)
}

/// `rdy new finding` and `rdy new source`: the `new_finding` and `new_source` ops. Both types
/// live only in the store, so the wrapper writes no working-copy file; like a profile, they go
/// to the view a human's working copy is bound to.
fn new_supporting(
    env: &Env,
    kind: &str,
    mut args: Value,
    view: Option<String>,
) -> Result<(Value, String), ErrorReply> {
    let view = match view {
        None if crate::conn::human_session(env) => crate::conn::bound_view(env)?,
        named => named,
    };
    args["view"] = json!(view);
    let v = call(env, &format!("new_{kind}"), args)?;
    let id = text(&v["created"][format!("${kind}")]);
    let mut t = format!("created {kind} {id}{}", brief_text::consequences_text(&v));
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    Ok((v, t))
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
    /// A source this edit follows (id, key or path); repeat for more.
    #[arg(long, value_name = "SOURCE", conflicts_with = "reason")]
    reflects: Vec<String>,
    /// Record the code state of the block, when it declares reflects globs.
    #[arg(long, conflicts_with = "reason")]
    reflects_code: bool,
}

/// `rdy new profile`: the `new_profile` op. A profile is supporting content, so the wrapper
/// writes no working-copy file and a copied worktree needs no refusal; like the docs it sits
/// beside, it goes to the view a human's working copy is bound to.
fn new_profile(
    env: &Env,
    args: Value,
    view: Option<String>,
) -> Result<(Value, String), ErrorReply> {
    let key = text(&args["key"]).to_string();
    let view = match view {
        None if crate::conn::human_session(env) => crate::conn::bound_view(env)?,
        named => named,
    };
    let mut args = args;
    args["view"] = json!(view);
    let v = call(env, "new_profile", args)?;
    let id = text(&v["created"]["$profile"]);
    let mut t = format!(
        "created publication profile {key} ({id}){}",
        brief_text::consequences_text(&v)
    );
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    Ok((v, t))
}

pub(crate) fn new(env: &Env, what: NewCmd) -> Result<(Value, String), ErrorReply> {
    let what = match what {
        NewCmd::Profile {
            key,
            parent,
            title,
            audience,
            select,
            description,
            trail,
            view,
        } => {
            // an absent option stays out of the request, so the op's own defaults hold
            let mut args = json!({
                "key": key, "parent": parent, "title": title,
                "audience": audience, "select": select,
            });
            if let Some(d) = description {
                args["description"] = json!(d);
            }
            if let Some(t) = trail {
                args["trail"] = json!(t);
            }
            return new_profile(env, args, view);
        }
        NewCmd::Finding {
            statement,
            quote,
            question,
            session,
            source,
            parent,
            view,
            at,
        } => {
            let evidence = evidence_entries(quote, question, session, source, &at)?;
            let mut args = json!({"statement": statement, "evidence": evidence});
            if let Some(p) = parent {
                args["parent"] = json!(p);
            }
            return new_supporting(env, "finding", args, view);
        }
        NewCmd::Source {
            title,
            kind,
            locator,
            version,
            parent,
            view,
        } => {
            let mut args = json!({"title": title, "kind": kind, "locator": locator});
            if let Some(v) = version {
                args["version"] = json!(v);
            }
            if let Some(p) = parent {
                args["parent"] = json!(p);
            }
            return new_supporting(env, "source", args, view);
        }
        other => other,
    };
    let named = match &what {
        NewCmd::Folder { view, .. } | NewCmd::Doc { view, .. } => view.clone(),
        NewCmd::Profile { .. } | NewCmd::Finding { .. } | NewCmd::Source { .. } => {
            unreachable!("handled above")
        }
    };
    wc::new_files::refuse_in_copy(env, named.as_deref())?;
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
        NewCmd::Profile { .. } | NewCmd::Finding { .. } | NewCmd::Source { .. } => {
            unreachable!("handled above")
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
    let EditArgs {
        id,
        view,
        reason,
        reflects,
        reflects_code,
    } = a;
    // rf8: the raw object rdy_write takes, as `rdy commit --reflects` sends it
    let reflects = (!reflects.is_empty() || reflects_code).then(|| {
        let mut o = serde_json::Map::new();
        if !reflects.is_empty() {
            o.insert("sources".into(), json!(reflects));
        }
        if reflects_code {
            o.insert("code".into(), json!(true));
        }
        Value::Object(o)
    });
    let v = edit(env, &id, view.as_deref(), reason.as_deref(), reflects)?;
    let t = write_text(&v);
    Ok((v, t))
}

/// A write's report as text: its change, the keys rdy assigned (F4, FW-PR3b: a rename or a
/// top-level name re-derives them), the consequences and the warnings. A person sees the
/// write's warnings, W_BRIEF_STALE among them (msw8, #874).
pub(crate) fn write_text(v: &Value) -> String {
    let mut t = format!(
        "change {}{}{}",
        hash_short(text(&v["change"])),
        keys_lines(v, ""),
        brief_text::consequences_text(v)
    );
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\nwarning {}: {}",
            text(&w["code"]),
            text(&w["message"])
        ));
    }
    t
}

/// `rdy edit`: the block as YAML from the `block` op, its briefing from the `brief` op shown as
/// `#` lines above the YAML, `$EDITOR` on a temp file, then the `edit_block` op citing the
/// briefing's token, the block's `version` as the base and its `conflict`, with `reason` as the
/// edit's editorial reason or `reflects` (rf8) as its raw reflects justification (a block or
/// conflict changed while the editor was open is
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
    reflects: Option<Value>,
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
            "reflects": reflects,
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
