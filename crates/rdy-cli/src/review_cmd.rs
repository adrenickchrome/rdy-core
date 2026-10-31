//! ck9 (D-JQD5X "Surfaces", n-XFJVW): `rdy review checks | run | request | list`, the CLI twin of
//! `rdy_review`. Each command builds the op-tagged request the ctl `review` op takes (the one
//! `rdy_review` builds), and the renderer turns the engine's reply into text. `--json` prints the
//! reply as it is. The run of a request comes from the session, as the relay table requires: the
//! command has no relayed-run flag, and `run` has no relay flags at all. `rdy check` stays the
//! working-copy check.
use crate::brief_text::one_line;
use crate::conn::{Env, call};
use crate::output::{num, text};
use clap::{Args, Subcommand};
use rdy_daemon::ErrorReply;
use rdy_engine::check::AgentCheck;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum ReviewCmd {
    /// List the checks, each with the subject it takes.
    Checks,
    /// Run a check on your own work: rdy writes an author-run request and prints its case.
    Run(RunArgs),
    /// Ask for an independent review of a check. A person's request through an agent carries
    /// the person's words (--relayed-by and --evidence).
    Request(RequestArgs),
    /// List requests with their verdicts.
    List(ListArgs),
}

/// The check and its subject: one value, or two (two block ids, or for diff_contradicts the
/// decision and the code range `<repo>@<from>..<to>`).
#[derive(Args)]
pub struct RunArgs {
    /// The check's name: rdy review checks lists them.
    check: String,
    /// One block, change or P-x@n; or the first of two values.
    subject: String,
    /// The second block id, or for diff_contradicts the code range <repo>@<from>..<to>.
    second: Option<String>,
}

#[derive(Args)]
pub struct RequestArgs {
    /// The check's name: rdy review checks lists them.
    check: String,
    /// One block, change or P-x@n; or the first of two values.
    subject: String,
    /// The second block id, or for diff_contradicts the code range <repo>@<from>..<to>.
    second: Option<String>,
    /// Why the review is wanted, 1 to 2000 characters.
    #[arg(long)]
    reason: Option<String>,
    /// The agent that relays the person's request: your own actor. The run comes from your session. Requires --evidence.
    #[arg(long, value_name = "ACTOR", requires = "evidence")]
    relayed_by: Option<String>,
    /// The person's words, quoted. Requires --relayed-by.
    #[arg(long, requires = "relayed_by")]
    evidence: Option<String>,
    /// The question the person was asked.
    #[arg(long, requires = "relayed_by")]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long = "session", value_name = "REF", requires = "relayed_by")]
    session_ref: Option<String>,
}

#[derive(Args)]
pub struct ListArgs {
    /// Only the requests whose subject this is: one block, change or P-x@n.
    #[arg(long)]
    subject: Option<String>,
    /// Only the open requests.
    #[arg(long)]
    open: bool,
}

/// The `subject` of a run or a request: the one string, or two values as a pair of block ids,
/// or for `diff_contradicts` as `{decision, code}`.
fn subject_of(check: &str, subject: &str, second: Option<&str>) -> Value {
    match second {
        None => json!(subject),
        Some(code) if AgentCheck::parse(check) == Some(AgentCheck::DiffContradicts) => {
            json!({"decision": subject, "code": code})
        }
        Some(second) => json!([subject, second]),
    }
}

/// The ctl `review` op's request for `cmd`.
pub fn args_of(cmd: &ReviewCmd) -> Value {
    match cmd {
        ReviewCmd::Checks => json!({"op": "checks"}),
        ReviewCmd::Run(a) => json!({
            "op": "run",
            "check": a.check,
            "subject": subject_of(&a.check, &a.subject, a.second.as_deref()),
        }),
        ReviewCmd::Request(a) => {
            let mut args = json!({
                "op": "request",
                "check": a.check,
                "subject": subject_of(&a.check, &a.subject, a.second.as_deref()),
            });
            for (k, v) in [
                ("reason", &a.reason),
                ("relayed_by", &a.relayed_by),
                ("evidence", &a.evidence),
                ("question", &a.question),
                ("session_ref", &a.session_ref),
            ] {
                if let Some(v) = v {
                    args[k] = json!(v);
                }
            }
            args
        }
        ReviewCmd::List(a) => {
            let mut args = json!({"op": "list"});
            if let Some(s) = &a.subject {
                args["subject"] = json!(s);
            }
            if a.open {
                args["open"] = json!(true);
            }
            args
        }
    }
}

/// A mode as the lines name it: an author's own run is `author-run`.
fn mode_text(mode: &str) -> &str {
    if mode == "author" { "author-run" } else { mode }
}

/// `<check>  <subject kind>` for each check, in catalogue order.
fn checks_text(v: &Value) -> String {
    v["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}  {}",
                one_line(text(&c["name"])),
                one_line(text(&c["subject"]))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// For each request, in the reply's order: `requested <request> (<check>, <mode>)`, `case
/// <case_hash>` and the pretty JSON of the case. With no request: `no related decision pairs`.
/// With pairs left out: `<n> related pairs omitted`.
fn requested_text(check: &str, mode: &str, v: &Value) -> String {
    let mut lines: Vec<String> = vec![];
    let rows = v["requests"].as_array().map_or(&[][..], |a| a);
    for r in rows {
        lines.push(format!(
            "requested {} ({}, {})",
            one_line(text(&r["request"])),
            one_line(check),
            mode_text(mode)
        ));
        lines.push(format!("case {}", one_line(text(&r["case_hash"]))));
        // pretty JSON escapes every control character
        lines.push(serde_json::to_string_pretty(&r["case"]).unwrap_or_default());
    }
    if rows.is_empty() {
        lines.push("no related decision pairs".to_string());
    }
    let omitted = num(&v["omitted"]);
    if omitted > 0 {
        lines.push(format!("{omitted} related pairs omitted"));
    }
    lines.join("\n")
}

/// `<request>  <check>  <mode>  <status>` for each request, each verdict on an indented line
/// below it as `  <review>  <verdict>  <actor>: <reason>` (no colon or reason when the reason is
/// empty); then `<n> requests omitted` when rows were left out.
fn list_text(v: &Value) -> String {
    let mut lines: Vec<String> = vec![];
    for r in v["requests"].as_array().into_iter().flatten() {
        lines.push(format!(
            "{}  {}  {}  {}",
            one_line(text(&r["request"])),
            one_line(text(&r["check"])),
            mode_text(text(&r["mode"])),
            one_line(text(&r["status"]))
        ));
        for d in r["verdicts"].as_array().into_iter().flatten() {
            let mut line = format!(
                "  {}  {}  {}",
                one_line(text(&d["review"])),
                one_line(text(&d["verdict"])),
                one_line(text(&d["actor"]))
            );
            if let Some(reason) = d["reason"].as_str().filter(|r| !r.is_empty()) {
                line.push_str(&format!(": {}", one_line(reason)));
            }
            lines.push(line);
        }
    }
    let omitted = num(&v["omitted"]);
    if omitted > 0 {
        lines.push(format!("{omitted} requests omitted"));
    }
    lines.join("\n")
}

pub(crate) fn review(env: &Env, cmd: ReviewCmd) -> Result<(Value, String), ErrorReply> {
    let v = call(env, "review", args_of(&cmd))?;
    let t = match &cmd {
        ReviewCmd::Checks => checks_text(&v),
        ReviewCmd::Run(a) => requested_text(&a.check, "author", &v),
        ReviewCmd::Request(a) => requested_text(&a.check, "independent", &v),
        ReviewCmd::List(_) => list_text(&v),
    };
    Ok((v, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case() -> Value {
        json!({"question": "q", "subject": {"blocks": ["D-1"]}})
    }

    #[test]
    fn ck9_requested_text_names_each_request_then_what_was_left_out() {
        let one = json!({"request": "rq-aaaaaaaaaaaa", "case_hash": "h1", "case": case(),
            "requests": [{"request": "rq-aaaaaaaaaaaa", "case_hash": "h1", "case": case()}], "omitted": 0});
        let pretty = serde_json::to_string_pretty(&case()).unwrap();
        assert_eq!(
            requested_text("decision_review", "author", &one),
            format!("requested rq-aaaaaaaaaaaa (decision_review, author-run)\ncase h1\n{pretty}")
        );
        let two = json!({"request": "rq-1", "case_hash": "h1", "case": case(),
            "requests": [{"request": "rq-1", "case_hash": "h1", "case": case()},
                         {"request": "rq-2", "case_hash": "h2", "case": case()}], "omitted": 3});
        assert_eq!(
            requested_text("contradiction", "independent", &two),
            format!(
                "requested rq-1 (contradiction, independent)\ncase h1\n{pretty}\nrequested rq-2 (contradiction, independent)\ncase h2\n{pretty}\n3 related pairs omitted"
            )
        );
        let none =
            json!({"request": null, "case_hash": null, "case": null, "requests": [], "omitted": 0});
        assert_eq!(
            requested_text("contradiction", "author", &none),
            "no related decision pairs"
        );
        let cut =
            json!({"request": null, "case_hash": null, "case": null, "requests": [], "omitted": 2});
        assert_eq!(
            requested_text("contradiction", "author", &cut),
            "no related decision pairs\n2 related pairs omitted"
        );
    }

    #[test]
    fn ck9_list_text_names_each_verdict_under_its_request() {
        let review = "a".repeat(64);
        let v = json!({"requests": [
            {"request": "rq-1", "check": "meaning_preservation", "mode": "independent", "status": "answered",
             "verdicts": [
                {"review": review, "verdict": "concern", "actor": "agent:r", "reason": "it reads\u{1b}[31m otherwise"},
                {"review": "b".repeat(64), "verdict": "pass", "actor": "agent:s", "reason": null}]},
            {"request": "rq-2", "check": "decision_review", "mode": "author", "status": "open", "verdicts": []}
        ], "omitted": 4});
        assert_eq!(
            list_text(&v),
            format!(
                "rq-1  meaning_preservation  independent  answered\n  {review}  concern  agent:r: it reads [31m otherwise\n  {}  pass  agent:s\nrq-2  decision_review  author-run  open\n4 requests omitted",
                "b".repeat(64)
            )
        );
        assert_eq!(list_text(&json!({"requests": [], "omitted": 0})), "");
    }

    #[test]
    fn ck9_two_values_are_a_pair_or_a_code_subject() {
        assert_eq!(subject_of("contradiction", "c-1", None), json!("c-1"));
        assert_eq!(
            subject_of("contradiction", "c-1", Some("c-2")),
            json!(["c-1", "c-2"])
        );
        assert_eq!(
            subject_of("diff_contradicts", "D-1", Some("app@a..b")),
            json!({"decision": "D-1", "code": "app@a..b"})
        );
    }
}
