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
    /// It lives only in the store. Omit parent to record it in the decision home, or at the
    /// project root when the project names none. The chat path is rdy_write.
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
        /// The doc, section or folder to record it in. Omit parent to record it in the decision
        /// home, or at the project root when the project names none.
        #[arg(long)]
        parent: Option<String>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
        /// Where each evidence option stood on the command line (`main` fills it).
        #[arg(skip)]
        at: EvidenceAt,
    },
    /// An instruction for agents (D-8X04F): one rule under a doc or section, the selectors
    /// that say where it applies, and the decision that is its reason. It gets no key. It goes
    /// to the session's view (`work` for the human, or the view your working copy is bound to).
    /// The chat path is rdy_write.
    Instruction {
        /// The doc or section to put it in (id, key or path).
        #[arg(long)]
        parent: String,
        /// The rule's whole text.
        #[arg(long)]
        text: String,
        /// A selector: all, area:<name>, type:<name> or act:<act>; repeat it for more.
        #[arg(long = "applies-to", value_name = "SELECTOR", required = true)]
        applies_to: Vec<String>,
        /// The decision that is the reason for the rule (id or key).
        #[arg(long, value_name = "DECISION")]
        reason: String,
        /// Store the instruction as locked.
        #[arg(long)]
        locked: bool,
    },
    /// A repo link: the alias this project uses for a cited project, the repo and the
    /// publication profile it delivers. It sits under a doc and gets no key. `rdy links` reads
    /// what it delivers. A person approves its landing: the authority policy never accepts it.
    /// The chat path is rdy_write.
    RepoLink {
        /// The alias: a lowercase letter, then up to 23 lowercase letters, digits or hyphens.
        alias: String,
        /// The doc to put it in (id, key or path).
        #[arg(long)]
        parent: String,
        /// The cited project's repo id: 32 lowercase hex digits.
        #[arg(long)]
        repo: String,
        /// The id of the publication profile the cited project delivers.
        #[arg(long)]
        profile: String,
        #[arg(long)]
        title: Option<String>,
        /// The view to write (default: the session's, `work` for the human).
        #[arg(long)]
        view: Option<String>,
    },
    /// A source: a release, commit or spec that a finding cites. It lives only in the store. Omit
    /// parent to record it in the decision home, or at the project root when the project names
    /// none. The chat path is rdy_write.
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
        /// The doc, section or folder to record it in. Omit parent to record it in the decision
        /// home, or at the project root when the project names none.
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
        None if crate::conn::human_session(env)? => crate::conn::bound_view(env)?,
        named => named,
    };
    args["view"] = json!(view);
    let v = call(env, &format!("new_{kind}"), args)?;
    let id = text(&v["created"][format!("${kind}")]);
    let mut t = format!(
        "created {kind} {id}{}{}",
        brief_text::consequences_text(&v),
        brief_text::created_briefing_text(&v)
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
        None if crate::conn::human_session(env)? => crate::conn::bound_view(env)?,
        named => named,
    };
    let mut args = args;
    args["view"] = json!(view);
    let v = call(env, "new_profile", args)?;
    let id = text(&v["created"]["$profile"]);
    let mut t = format!(
        "created publication profile {key} ({id}){}{}",
        brief_text::consequences_text(&v),
        brief_text::created_briefing_text(&v)
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
        NewCmd::RepoLink {
            alias,
            parent,
            repo,
            profile,
            title,
            view,
        } => {
            let mut args = json!({"alias": alias, "parent": parent, "repo": repo,
                "profile": profile});
            if let Some(t) = title {
                args["title"] = json!(t);
            }
            return new_supporting(env, "repo_link", args, view);
        }
        NewCmd::Instruction {
            parent,
            text,
            applies_to,
            reason,
            locked,
        } => {
            // ain5: `locked` only when set; the view is the session's own, as for a finding
            let mut args = json!({"parent": parent, "text": text, "applies_to": applies_to,
                "reason": reason});
            if locked {
                args["locked"] = json!(true);
            }
            return new_supporting(env, "instruction", args, None);
        }
        other => other,
    };
    let named = match &what {
        NewCmd::Folder { view, .. } | NewCmd::Doc { view, .. } => view.clone(),
        NewCmd::Profile { .. }
        | NewCmd::Finding { .. }
        | NewCmd::Source { .. }
        | NewCmd::RepoLink { .. }
        | NewCmd::Instruction { .. } => {
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
        NewCmd::Profile { .. }
        | NewCmd::Finding { .. }
        | NewCmd::Source { .. }
        | NewCmd::RepoLink { .. }
        | NewCmd::Instruction { .. } => {
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
        "created {kind} {path} ({last}){}{}",
        brief_text::consequences_text(&v),
        brief_text::created_briefing_text(&v)
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
        "change {}{}{}{}",
        hash_short(text(&v["change"])),
        keys_lines(v, ""),
        brief_text::consequences_text(v),
        brief_text::created_briefing_text(v)
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
/// `#` lines above the YAML, the editor on a temp file, then the `edit_block` op citing the
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
    // The handle closes at once, so an editor on Windows can open, write and replace the file.
    let tmp = tempfile::Builder::new()
        .prefix("rdy-edit-")
        .suffix(".yaml")
        .tempfile()
        .map_err(|e| io_error(e, "check the temp directory"))?
        .into_temp_path();
    std::fs::write(
        &tmp,
        format!("{}{}", brief_text::header(&briefing), text(&block["yaml"])),
    )
    .map_err(|e| io_error(e, "check the temp directory"))?;
    let status = run_editor(&tmp)?;
    if !status.success() {
        return Err(ErrorReply::new(
            E_BAD_INPUT,
            "editor exited with an error; nothing changed",
            "save and quit the editor normally",
        ));
    }
    let yaml = std::fs::read_to_string(&tmp).map_err(|e| io_error(e, "retry"))?;
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
        if let Ok(path) = tmp.keep() {
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

/// Unix: `$EDITOR` (default `vi`) through `sh`, with the file as `$1`.
#[cfg(not(windows))]
fn run_editor(path: &std::path::Path) -> Result<std::process::ExitStatus, ErrorReply> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    std::process::Command::new("sh")
        .arg("-c")
        .arg("$EDITOR \"$1\"")
        .arg("sh")
        .arg(path)
        .env("EDITOR", &editor)
        .status()
        .map_err(|e| ErrorReply::from(RdyError::new(E_IO, format!("{editor}: {e}"), "set $EDITOR")))
}

/// Windows: `%VISUAL%`, else `%EDITOR%`, else `notepad.exe`, started directly (never through
/// `cmd.exe` or `sh`) with the file path as one last argument.
#[cfg(windows)]
fn run_editor(path: &std::path::Path) -> Result<std::process::ExitStatus, ErrorReply> {
    let (name, value) = editor_choice(|n| std::env::var_os(n).map(|v| v.to_string_lossy().into()));
    let argv = split_command_line(&value).map_err(|_| bad_editor(&name, &value))?;
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| bad_editor(&name, &value))?;
    std::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .map_err(|e| {
            ErrorReply::from(RdyError::new(
                E_IO,
                format!("{program}: {e}"),
                "set %VISUAL% or %EDITOR%",
            ))
        })
}

/// The editor variable and its value: `VISUAL`, else `EDITOR`, else `notepad.exe`. A variable
/// that is unset, empty or only white space counts as unset.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn editor_choice(get: impl Fn(&str) -> Option<String>) -> (String, String) {
    for name in ["VISUAL", "EDITOR"] {
        if let Some(v) = get(name).filter(|v| !v.trim().is_empty()) {
            return (name.into(), v);
        }
    }
    ("EDITOR".into(), "notepad.exe".into())
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn bad_editor(name: &str, value: &str) -> ErrorReply {
    ErrorReply::new(
        E_BAD_INPUT,
        format!("the editor command in %{name}% cannot be parsed: {value}"),
        "set %VISUAL% or %EDITOR% to a program, in double quotes if its path has spaces, with any arguments after it",
    )
}

/// Splits a command line by the Windows rules: items end at white space outside double quotes;
/// `2n` backslashes before a quote give `n` backslashes and the quote toggles; `2n+1` give `n`
/// backslashes and a literal quote; other backslashes stay. An unclosed quote is an error.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn split_command_line(s: &str) -> Result<Vec<String>, ()> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut started, mut quoted, mut slashes) = (false, false, 0usize);
    for c in s.chars() {
        match c {
            '\\' => {
                slashes += 1;
                started = true;
                continue;
            }
            '"' => {
                cur.extend(std::iter::repeat_n('\\', slashes / 2));
                if slashes % 2 == 1 {
                    cur.push('"');
                } else {
                    quoted = !quoted;
                }
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                cur.extend(std::iter::repeat_n('\\', slashes));
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.extend(std::iter::repeat_n('\\', slashes));
                cur.push(c);
                started = true;
            }
        }
        slashes = 0;
    }
    if quoted {
        return Err(());
    }
    cur.extend(std::iter::repeat_n('\\', slashes));
    if started {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod wx20_tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |n| {
            pairs
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn wx20_visual_precedes_editor_and_notepad() {
        let both = [("VISUAL", "v.exe"), ("EDITOR", "e.exe")];
        assert_eq!(editor_choice(env(&both)), ("VISUAL".into(), "v.exe".into()));
        for blank in ["", "   ", "\t "] {
            let p = [("VISUAL", blank), ("EDITOR", "e.exe")];
            assert_eq!(editor_choice(env(&p)), ("EDITOR".into(), "e.exe".into()));
            let p = [("VISUAL", blank), ("EDITOR", blank)];
            assert_eq!(editor_choice(env(&p)).1, "notepad.exe");
        }
        assert_eq!(editor_choice(env(&[])).1, "notepad.exe");
    }

    #[test]
    fn wx20_editor_receives_one_literal_native_path() {
        let s =
            split_command_line(r#""C:\Program Files\Ed it\ed.exe" -w --x="a b" plain"#).unwrap();
        assert_eq!(
            s,
            [r"C:\Program Files\Ed it\ed.exe", "-w", "--x=a b", "plain"]
        );
        // the path is never part of the parse: it is appended as its own item
        let path = std::path::Path::new(r"C:\Users\Zoë & 100%\rdy-edit-1.yaml");
        let mut argv = split_command_line("ed.exe").unwrap();
        argv.push(path.to_string_lossy().into());
        assert_eq!(argv[1], r"C:\Users\Zoë & 100%\rdy-edit-1.yaml");
        assert_eq!(argv.len(), 2);
        // backslash rules
        assert_eq!(split_command_line(r#"a\\"b c""#).unwrap(), [r"a\b c"]);
        assert_eq!(split_command_line(r#"a\"b"#).unwrap(), [r#"a"b"#]);
        assert_eq!(split_command_line(r"C:\dir\").unwrap(), [r"C:\dir\"]);
        // refused: an unclosed quote, and no program
        assert!(split_command_line(r#""C:\Program Files\ed.exe"#).is_err());
        assert!(split_command_line("  ").unwrap().is_empty());
        let e = bad_editor("VISUAL", "\"x");
        assert_eq!(e.code, E_BAD_INPUT);
        assert_eq!(
            e.message,
            "the editor command in %VISUAL% cannot be parsed: \"x"
        );
        assert_eq!(
            e.fix,
            "set %VISUAL% or %EDITOR% to a program, in double quotes if its path has spaces, with any arguments after it"
        );
    }

    #[test]
    fn wx20_editor_can_reopen_the_closed_temp_file() {
        let tmp = tempfile::Builder::new()
            .prefix("rdy-edit-")
            .suffix(".yaml")
            .tempfile()
            .unwrap()
            .into_temp_path();
        std::fs::write(&tmp, "a").unwrap();
        // another handle opens, replaces and renames over it, as an editor would
        let mut f = std::fs::OpenOptions::new().write(true).open(&tmp).unwrap();
        std::io::Write::write_all(&mut f, b"b").unwrap();
        drop(f);
        let side = tmp.with_extension("new");
        std::fs::write(&side, "c").unwrap();
        std::fs::rename(&side, &tmp).unwrap();
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "c");
        let kept = tmp.keep().unwrap();
        assert!(kept.exists());
        std::fs::remove_file(kept).unwrap();
    }
}
