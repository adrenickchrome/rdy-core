//! `rdy push` and `rdy pull` (D-47HVF, D-3RSCW, D-KQ4CA, D-VRH3Z): the command-line fallback of
//! `rdy_push` and `rdy_pull`, through the daemon's sync ops and so the one sync coordinator.
//! With `--no-daemon` the op runs in this process by the direct route every op takes. The hub
//! credentials are the daemon process's (with `--no-daemon`, this process's): after changing
//! `XDG_CONFIG_HOME`, run `rdy serve --stop`.
//!
//! - **A person's card.** A forced push that moves hub canon, `--restore` into a hub repo that
//!   holds canon and `--take` of hub canon each get a card first. On a terminal, with none of
//!   `--json`, `--card-hash`, `--answer` and `--relayed-by`, the command prints the card and asks
//!   in place, as `rdy auth sign-in` does (D-VRH3Z); the answer goes through the same relay check
//!   as `--answer`. Otherwise the card is the reply: answer it with the relay flags.
//! - **A hub dump.** `rdy pull --dump <id> --out <file> [--remote <name> | --url <url>]`
//!   fetches the hub's dump to a new file (D-7CDEQ): no role, no lock, no store write.
//! - **The reply is the engine's.** The text relays what the op returned: the lookup of an
//!   earlier write, each end, the partial results of a pull, a restore's omissions. `--json`
//!   prints the reply unchanged.
use crate::auth_cmd::{AnswerArg, Console, StdConsole, ask, is_yes};
use crate::conn::{Env, call};
use crate::output::{hash_short, num, text};
use rdy_daemon::ErrorReply;
use serde_json::{Value, json};

/// The relay flags of a card answer, as `rdy auth sign-in` takes them.
#[derive(clap::Args, Clone, Default)]
pub(crate) struct RelayArgs {
    /// The card hash the first call printed, to answer that card.
    #[arg(long)]
    card_hash: Option<String>,
    /// The person's answer to the card: confirm or cancel. Needs --card-hash.
    #[arg(long, value_enum)]
    answer: Option<AnswerArg>,
    /// The agent that relays the person's words: your own actor. The run comes from your session.
    #[arg(long, value_name = "ACTOR")]
    relayed_by: Option<String>,
    /// The person's words, quoted.
    #[arg(long)]
    evidence: Option<String>,
    /// The question the person was asked.
    #[arg(long)]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long)]
    session_ref: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct PushArgs {
    /// The share remote to push to (default: the share remote, else the only remote).
    #[arg(long)]
    remote: Option<String>,
    /// A view to push after canon; repeat for more (at most 256, each once).
    #[arg(long = "view", value_name = "NAME")]
    views: Vec<String>,
    /// Force hub canon to this project's head when local canon does not hold the hub tip. A card comes first.
    #[arg(long)]
    force: bool,
    /// Restore a dump into the hub repo instead of pushing. Takes no --view and no --force.
    #[arg(long, value_name = "DUMP")]
    restore: Option<String>,
    #[command(flatten)]
    relay: RelayArgs,
}

#[derive(clap::Args)]
pub(crate) struct PullArgs {
    /// The share remote to pull from (default: the share remote, else the only remote).
    #[arg(long)]
    remote: Option<String>,
    /// A view to pull after canon; repeat for more (at most 256, each once).
    #[arg(long = "view", value_name = "NAME")]
    views: Vec<String>,
    /// Take hub canon with no replay (a card comes first); with --view, take only those views.
    #[arg(long)]
    take: bool,
    /// Replay none of your unpushed landings: canon takes the hub tip and they go into a new view pull-<n>. Not with --take and no --view.
    #[arg(long)]
    strict: bool,
    /// Fetch the hub dump with this id (the id the hub's dump job gave) to --out instead of pulling. Takes only --out and one of --remote or --url.
    #[arg(long, value_name = "ID")]
    dump: Option<String>,
    /// The new file a dump fetch writes. Only with --dump.
    #[arg(long, value_name = "FILE")]
    out: Option<std::path::PathBuf>,
    /// The https URL of the hub repo a dump fetch reads, in place of --remote; it needs no project. Only with --dump.
    #[arg(long, value_name = "URL")]
    url: Option<String>,
    #[command(flatten)]
    relay: RelayArgs,
}

fn put_relay(args: &mut Value, r: &RelayArgs) {
    let answer = r.answer.map(|a| match a {
        AnswerArg::Confirm => "confirm",
        AnswerArg::Cancel => "cancel",
    });
    for (k, v) in [
        ("card_hash", r.card_hash.clone()),
        ("answer", answer.map(str::to_string)),
        ("relayed_by", r.relayed_by.clone()),
        ("evidence", r.evidence.clone()),
        ("question", r.question.clone()),
        ("session_ref", r.session_ref.clone()),
    ] {
        if let Some(v) = v {
            args[k] = json!(v);
        }
    }
}

/// The `push` op's arguments: each flag only when it is given.
pub(crate) fn push_request(a: &PushArgs) -> Value {
    let mut args = json!({});
    if let Some(r) = &a.remote {
        args["remote"] = json!(r);
    }
    if !a.views.is_empty() {
        args["views"] = json!(a.views);
    }
    if a.force {
        args["force"] = json!(true);
    }
    if let Some(d) = &a.restore {
        args["restore"] = json!(d);
    }
    put_relay(&mut args, &a.relay);
    args
}

/// The `pull` op's arguments: each flag only when it is given.
pub(crate) fn pull_request(a: &PullArgs) -> Value {
    let mut args = json!({});
    if let Some(r) = &a.remote {
        args["remote"] = json!(r);
    }
    if !a.views.is_empty() {
        args["views"] = json!(a.views);
    }
    if a.take {
        args["take"] = json!(true);
    }
    if a.strict {
        args["strict"] = json!(true);
    }
    if let Some(d) = &a.dump {
        args["dump"] = json!(d);
    }
    if let Some(o) = &a.out {
        args["out"] = json!(o.to_string_lossy());
    }
    if let Some(u) = &a.url {
        args["url"] = json!(u);
    }
    put_relay(&mut args, &a.relay);
    args
}

fn relayed_already(r: &RelayArgs) -> bool {
    r.card_hash.is_some() || r.answer.is_some() || r.relayed_by.is_some()
}

/// Whether the card is answered in place: a card reply, none of `--json` and the relay flags,
/// and a terminal on both standard streams.
fn in_place(v: &Value, as_json: bool, r: &RelayArgs, console: &dyn Console) -> bool {
    v["status"] == "confirmation_required"
        && !as_json
        && !relayed_already(r)
        && console.stdin_is_terminal()
        && console.stdout_is_terminal()
}

/// Runs `op` with `args` through `send`; a card reply that [`in_place`] allows is printed and
/// answered in place: `y` or `yes` confirms, anything else cancels. The answer is sent with the
/// card's hash, as `--answer` sends it.
fn run_op(
    send: &mut dyn FnMut(Value) -> Result<Value, ErrorReply>,
    mut args: Value,
    as_json: bool,
    r: &RelayArgs,
    console: &mut dyn Console,
) -> Result<Value, ErrorReply> {
    let v = send(args.clone())?;
    if !in_place(&v, as_json, r, console) {
        return Ok(v);
    }
    console.write(text(&v["card"]));
    let line = ask(console, "Move canon as the card says? [y/N] ");
    let answer = if line.as_deref().is_some_and(is_yes) {
        "confirm"
    } else {
        "cancel"
    };
    args["card_hash"] = v["card_hash"].clone();
    args["answer"] = json!(answer);
    send(args)
}

/// `rdy push`: the `push` op's reply and its text.
pub(crate) fn push(env: &Env, a: PushArgs, as_json: bool) -> Result<(Value, String), ErrorReply> {
    let v = run_op(
        &mut |args| call(env, "push", args),
        push_request(&a),
        as_json,
        &a.relay,
        &mut StdConsole,
    )?;
    let t = push_text(&v);
    Ok((v, t))
}

/// `rdy pull --dump`: the fetch of a hub dump (D-7CDEQ). Its closed inputs are checked first.
/// In a project it runs through the daemon's `pull` op; a `--url` fetch outside one runs in this
/// process with the user config's trust and sign-in, as `rdy clone <url>` runs. Its text names
/// the file as `rdy dump` reports it, then the next step.
fn pull_dump(env: &Env, a: &PullArgs) -> Result<(Value, String), ErrorReply> {
    let args = pull_request(a);
    let given: Vec<&str> = args
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    rdy_mcp::tools::check_dump_inputs(&given)?;
    let in_project = env.project.join(".rdy").join("store").exists();
    let v = match (&a.url, &a.dump, &a.out) {
        (Some(url), Some(dump), Some(out)) if !in_project => {
            rdy_engine::sync_dump::check_dump_id(dump)?;
            let user_file = rdy_engine::hub_credentials::require_user_file(
                rdy_engine::config_levels::user_file_from_env(),
            )?;
            let person = rdy_mcp::tools::auth_person(None, &user_file)?;
            let (actor, run) = crate::conn::session_env()?;
            let session = rdy_engine::classify(
                &person,
                actor.as_deref(),
                run.as_deref(),
                rdy_model::Role::Reader,
            )?;
            let out = env.cwd.join(out);
            let f = rdy_engine::sync_dump::fetch_dump_url(&session, url, dump, &out, &user_file)?;
            serde_json::to_value(&f).expect("a reply serializes")
        }
        _ => call(env, "pull", args)?,
    };
    let t = format!(
        "fetched hub dump to {} ({} bytes): dump format {}, {} dump\nrepo id: {}\ndigest: {}\nnext: {}",
        text(&v["out"]),
        num(&v["bytes"]),
        num(&v["dump_format"]),
        text(&v["kind"]),
        text(&v["repo_id"]),
        text(&v["digest"]),
        text(&v["next"])
    );
    Ok((v, t))
}

/// `rdy pull`: the `pull` op's reply and its text.
pub(crate) fn pull(env: &Env, a: PullArgs, as_json: bool) -> Result<(Value, String), ErrorReply> {
    if a.dump.is_some() || a.out.is_some() || a.url.is_some() {
        return pull_dump(env, &a);
    }
    let v = run_op(
        &mut |args| call(env, "pull", args),
        pull_request(&a),
        as_json,
        &a.relay,
        &mut StdConsole,
    )?;
    let t = pull_text(&v);
    Ok((v, t))
}

fn tip(v: &Value) -> String {
    v.as_str()
        .map_or_else(|| "none".to_string(), |h| hash_short(h).to_string())
}

/// The lines of a card reply: the card, its hash and how to answer it.
fn card_text(v: &Value) -> String {
    format!(
        "{}card hash: {}\nanswer with --answer and --card-hash",
        text(&v["card"]),
        text(&v["card_hash"])
    )
}

/// A refusal's line: its code, message and fix, as the hub or rdy gave them.
fn refusal_line(label: &str, e: &Value) -> String {
    format!(
        "{label} refused: {}: {}; {}",
        text(&e["code"]),
        text(&e["message"]),
        text(&e["fix"])
    )
}

/// The lines of one write end of a push or restore, under `label`.
fn end_text(label: &str, end: &Value) -> String {
    match text(&end["status"]) {
        "up_to_date" => format!("{label} is up to date at {}", tip(&end["tip"])),
        "taken" => format!(
            "{label} taken: {} on hub tip {} ({} batches, {} records)",
            tip(&end["head"]),
            tip(&end["tip"]),
            num(&end["sent"]["batches"]),
            num(&end["sent"]["records"])
        ),
        "held" => format!(
            "{label} held as landing request {} (version {})",
            num(&end["request"]["number"]),
            num(&end["request"]["version"])
        ),
        "held_open" => format!("{label} waits in landing request {}", num(&end["request"])),
        "refused" => refusal_line(label, &end["error"]),
        other => format!("{label}: {other}"),
    }
}

/// The line of the outcome lookup that ran before a push or pull.
fn lookup_text(l: &Value) -> Option<String> {
    if l.is_null() {
        return None;
    }
    let r = &l["result"];
    Some(format!(
        "earlier {} write {}: {}",
        text(&l["kind"]),
        text(&l["push"]),
        text(&r["outcome"])
    ))
}

fn push_text(v: &Value) -> String {
    match text(&v["status"]) {
        "confirmation_required" => return card_text(v),
        "cancelled" => {
            return format!(
                "cancelled: hub canon on {} is unchanged",
                text(&v["remote"])
            );
        }
        _ => {}
    }
    let mut lines = vec![format!("remote {}", text(&v["remote"]))];
    if v["holder"].is_boolean() {
        // a restore: the canon end, each view, and what the restore did not do
        if let Some(l) = v["lookups"].as_array() {
            for x in l {
                lines.push(format!(
                    "lookup of push {}: {}",
                    text(&x["push"]),
                    text(&x["result"]["outcome"])
                ));
            }
        }
        if !v["canon"].is_null() {
            lines.push(end_text("canon", &v["canon"]));
        }
        for row in v["views"].as_array().into_iter().flatten() {
            lines.push(end_text(
                &format!("view {}", text(&row["view"])),
                &row["end"],
            ));
        }
        for t in v["without_inputs"].as_array().into_iter().flatten() {
            lines.push(format!("tag {} was sent without render inputs", text(t)));
        }
        for k in v["kept"].as_array().into_iter().flatten() {
            lines.push(format!("hub view {} was left as it is", text(k)));
        }
        if let Some(c) = v["cleanup"].as_str() {
            lines.push(format!("cleanup: {c}"));
        }
        return lines.join("\n");
    }
    if let Some(l) = lookup_text(&v["lookup"]) {
        lines.push(l);
    }
    for n in v["notes"].as_array().into_iter().flatten() {
        lines.push(text(n).to_string());
    }
    lines.push(end_text("canon", &v["canon"]));
    for row in v["views"].as_array().into_iter().flatten() {
        lines.push(end_text(
            &format!("view {}", text(&row["view"])),
            &row["end"],
        ));
    }
    lines.join("\n")
}

fn tag_names(v: &Value) -> (usize, String) {
    let names: Vec<&str> = v.as_array().into_iter().flatten().map(text).collect();
    let list = if names.is_empty() {
        String::new()
    } else {
        format!(" ({})", names.join(", "))
    };
    (names.len(), list)
}

fn fetched_text(f: &Value) -> String {
    let (taken, taken_names) = tag_names(&f["tags_taken"]);
    let (kept, kept_names) = tag_names(&f["tags_kept"]);
    format!(
        "{} records taken, {taken} tags taken{taken_names}, {kept} tags kept{kept_names}",
        num(&f["records_taken"]),
    )
}

fn pull_text(v: &Value) -> String {
    match text(&v["status"]) {
        "confirmation_required" => return card_text(v),
        "cancelled" => {
            return format!(
                "cancelled: local canon is unchanged ({})",
                text(&v["remote"])
            );
        }
        _ => {}
    }
    let mut lines = vec![format!("remote {}", text(&v["remote"]))];
    if let Some(l) = lookup_text(&v["lookup"]) {
        lines.push(l);
    }
    if v["fetched"].is_object() {
        lines.push(format!("canon fetched: {}", fetched_text(&v["fetched"])));
    }
    let c = &v["canon"];
    if c.is_object() {
        lines.push(format!(
            "canon {}: now {} (hub tip {})",
            text(&c["status"]),
            tip(&c["canon"]),
            tip(&c["hub_tip"])
        ));
        for r in c["replayed"].as_array().into_iter().flatten() {
            lines.push(format!("  replayed {}", tip(&r["landing"])));
        }
        for n in c["not_replayed"].as_array().into_iter().flatten() {
            lines.push(format!("  not replayed: {n}"));
        }
        if let Some(view) = c["view"].as_str() {
            lines.push(format!(
                "  the changes that did not replay are in view {view}"
            ));
        }
    }
    for row in v["views_fetched"].as_array().into_iter().flatten() {
        lines.push(format!(
            "view {} fetched: {}",
            text(&row["view"]),
            fetched_text(&row["fetched"])
        ));
    }
    for p in v["views"].as_array().into_iter().flatten() {
        let mut line = format!("view {}: {}", text(&p["view"]), text(&p["status"]));
        if let Some(h) = p["head"].as_str() {
            line.push_str(&format!(", now {}", hash_short(h)));
        }
        lines.push(line);
        if p["refusal"].is_object() {
            lines.push(refusal_line(
                &format!("  view {}", text(&p["view"])),
                &p["refusal"],
            ));
        }
        for conflict in p["conflicts"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  conflict {} on {}",
                text(&conflict["id"]),
                text(&conflict["block"])
            ));
        }
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{}: {}\nfix: {}",
            text(&w["code"]),
            text(&w["message"]),
            text(&w["fix"])
        ));
    }
    lines.join("\n")
}

/// The status lines of one remote's sync state: what was fetched and applied, a held request,
/// a write waiting for its reply, an upload not sent, the landings not sent, and each view.
/// Each line starts with a newline.
pub(crate) fn status_lines(row: &Value) -> String {
    let remote = text(&row["remote"]);
    let mut out = format!(
        "\nsync {remote}: fetched {}, applied {}, {} unsent landings",
        tip(&row["fetched"]),
        tip(&row["applied"]),
        num(&row["unsent"])
    );
    if row["held"].is_object() {
        out.push_str(&format!(
            "\nsync {remote}: landing request {} holds {} landings",
            num(&row["held"]["request"]),
            num(&row["held_landings"])
        ));
    }
    if row["pending"].is_object() {
        out.push_str(&format!(
            "\nsync {remote}: the write of push {} has no reply yet",
            text(&row["pending"]["push"])
        ));
    }
    if row["upload"].is_object() {
        out.push_str(&format!(
            "\nsync {remote}: the upload of push {} was not sent",
            text(&row["upload"]["push"])
        ));
    }
    for v in row["views"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\nsync {remote} view {}: fetched {}, applied {}, {} unsent ops",
            text(&v["view"]),
            tip(&v["fetched"]),
            tip(&v["applied"]),
            num(&v["unsent"])
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A terminal of the test's making: the two terminal flags, the lines it gives and
    /// everything written to it.
    struct Fake {
        tty: bool,
        lines: VecDeque<String>,
        out: String,
    }

    impl Fake {
        fn new(tty: bool, lines: &[&str]) -> Fake {
            Fake {
                tty,
                lines: lines.iter().map(|l| l.to_string()).collect(),
                out: String::new(),
            }
        }
    }

    impl Console for Fake {
        fn stdin_is_terminal(&self) -> bool {
            self.tty
        }
        fn stdout_is_terminal(&self) -> bool {
            self.tty
        }
        fn read_line(&mut self) -> Option<String> {
            self.lines.pop_front()
        }
        fn write(&mut self, text: &str) {
            self.out.push_str(text);
        }
    }

    fn card() -> Value {
        json!({"status": "confirmation_required", "card": "Force the hub's canon\n",
            "card_hash": "h1"})
    }

    /// Sends the op's arguments to a fake daemon that answers a card, then the move.
    fn drive(console: &mut Fake, as_json: bool, relay: &RelayArgs) -> (Value, Vec<Value>) {
        let mut seen = Vec::new();
        let v = run_op(
            &mut |args| {
                seen.push(args.clone());
                Ok(if args.get("card_hash").is_some() {
                    json!({"status": "moved", "answer": args["answer"]})
                } else {
                    card()
                })
            },
            json!({"force": true}),
            as_json,
            relay,
            console,
        )
        .unwrap();
        (v, seen)
    }

    /// scc9 (terminal part): on a terminal the card is printed and answered in place; `y` or
    /// `yes` confirms, anything else cancels; the answer carries the card's hash.
    #[test]
    fn scc9_terminal_answers_the_card_in_place() {
        for (line, want) in [
            ("y", "confirm"),
            ("YES", "confirm"),
            ("", "cancel"),
            ("no", "cancel"),
        ] {
            let mut c = Fake::new(true, &[line]);
            let (v, seen) = drive(&mut c, false, &RelayArgs::default());
            assert_eq!(v["answer"], want, "{line:?}");
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[1]["card_hash"], "h1");
            assert!(c.out.starts_with("Force the hub's canon\n"), "{}", c.out);
            assert!(c.out.contains("[y/N]"), "{}", c.out);
        }
        // the end of input cancels
        let mut c = Fake::new(true, &[]);
        assert_eq!(
            drive(&mut c, false, &RelayArgs::default()).0["answer"],
            "cancel"
        );
    }

    /// scc9 (terminal part): `--json`, a relay flag or a stream that is not a terminal keep the
    /// card as the reply: the command asks nothing and sends nothing more.
    #[test]
    fn scc9_flags_or_no_terminal_keep_the_card_path() {
        let answered = RelayArgs {
            card_hash: Some("h0".into()),
            ..RelayArgs::default()
        };
        for (tty, json, relay) in [
            (true, true, RelayArgs::default()),
            (true, false, answered.clone()),
            (false, false, RelayArgs::default()),
        ] {
            let mut c = Fake::new(tty, &["y"]);
            let (v, seen) = drive(&mut c, json, &relay);
            assert_eq!(v["status"], "confirmation_required");
            assert_eq!(seen.len(), 1);
            assert!(c.out.is_empty(), "{}", c.out);
        }
    }

    #[test]
    fn scc9_fetched_text_lists_taken_and_kept_tags() {
        let f = json!({"records_taken": 5, "tags_taken": ["v1", "v2"], "tags_kept": ["v0"]});
        assert_eq!(
            fetched_text(&f),
            "5 records taken, 2 tags taken (v1, v2), 1 tags kept (v0)"
        );
        let none = json!({"records_taken": 0, "tags_taken": [], "tags_kept": []});
        assert_eq!(
            fetched_text(&none),
            "0 records taken, 0 tags taken, 0 tags kept"
        );
    }

    #[test]
    fn scc9_card_and_status_text() {
        assert_eq!(
            push_text(&card()),
            "Force the hub's canon\ncard hash: h1\nanswer with --answer and --card-hash"
        );
        let row = json!({"remote": "origin", "fetched": null, "applied": "0123456789abcdef",
            "unsent": 2, "held": {"request": 4}, "held_landings": 1, "pending": {"push": "p1"},
            "views": [{"view": "main", "fetched": null, "applied": null, "unsent": 3}]});
        let t = status_lines(&row);
        for line in [
            "sync origin: fetched none, applied 0123456789ab, 2 unsent landings",
            "sync origin: landing request 4 holds 1 landings",
            "sync origin: the write of push p1 has no reply yet",
            "sync origin view main: fetched none, applied none, 3 unsent ops",
        ] {
            assert!(t.contains(line), "{t}");
        }
    }
}
