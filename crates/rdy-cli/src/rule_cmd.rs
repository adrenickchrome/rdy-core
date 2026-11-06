//! `rdy rule add|edit|remove|answer` (FW-MH.rules, D-ZSRBN): the CLI twin of `rdy_rule`. The chat
//! path is primary; this is its fallback, with the same engine, the same request and the same
//! reply. An agent session relays the person's words (`--relayed-by` with `--evidence`); the CLI
//! has no Later.
//!
//! The request is checked here first ([`rdy_engine::rule_types::parse_request`], then the relay
//! table), so a bad form never starts a daemon or opens a store. Then the request goes where it
//! belongs ([`rdy_engine::rules::rule_route`]): a rule of this project, or of the meta project
//! when this process is in it, through the project's ctl op `rule`; a person's rule from another
//! project or from none, straight to the meta project through the person host. A project rule
//! outside every project is the engine's `E_NO_PROJECT`.
use crate::conn::{Env, call, session_env};
use crate::instructions_cmd::person_reader;
use crate::output::text;
use rdy_daemon::ErrorReply;
use rdy_engine::answer::relay::{RelayInput, relayed};
use rdy_engine::rule_types::{RuleRequest, parse_request};
use rdy_engine::rules::{RuleOrigin, RuleRoute, route_rule, rule_route};
use rdy_engine::signing_crypto::SigningAgent;
use rdy_engine::{E_BAD_INPUT, classify, human_actor_for_env};
use rdy_model::Role;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(clap::Subcommand)]
pub(crate) enum RuleCmd {
    /// Add a rule from the person's words: rdy rule add --scope <person|project> --text <text>
    /// --why <reason> --evidence <words> --relayed-by <actor>. Your own rules go to the meta
    /// project; project rules go to this project. The chat path is rdy_rule.
    Add(AddArgs),
    /// Change the wording of a rule from the person's words. The chat path is rdy_rule.
    Edit(EditArgs),
    /// Retire a rule from the person's words. The chat path is rdy_rule.
    Remove(RemoveArgs),
    /// Answer a card that a rule command returned: rdy rule answer <ref> <option> --scope <scope>
    /// --card <hash> --evidence <words> --relayed-by <actor>. The CLI has no Later.
    Answer(AnswerArgs),
}

/// The words every form that changes a rule shares.
#[derive(clap::Args)]
pub(crate) struct Words {
    /// The reason, in one line. It becomes the reason of the rule's decision.
    #[arg(long, value_name = "REASON")]
    why: String,
    /// The person's words, quoted.
    #[arg(long, value_name = "WORDS")]
    evidence: String,
    /// The agent that relays the person's words: your own actor. The run comes from your session.
    #[arg(long, value_name = "ACTOR")]
    relayed_by: String,
    /// The question the person was asked.
    #[arg(long, value_name = "TEXT")]
    question: Option<String>,
    /// Where the person said it (default: cli).
    #[arg(long = "session", value_name = "REF")]
    session_ref: Option<String>,
}

/// The parts of a rule's content.
#[derive(clap::Args)]
pub(crate) struct Content {
    /// The rule in full.
    #[arg(long, value_name = "TEXT")]
    text: String,
    /// The ruleset that holds the rule: a doc id or path in the scope, or its person: or
    /// project: handle. Default for an add: the scope's default ruleset.
    #[arg(long, value_name = "DOC")]
    ruleset: Option<String>,
    /// When the rule applies, as selectors. Default for an add: all.
    #[arg(long, num_args = 1.., value_name = "SELECTOR")]
    applies_to: Option<Vec<String>>,
}

#[derive(clap::Args)]
pub(crate) struct AddArgs {
    /// Where the rule lives: your own rules (person), or this project (project).
    #[arg(long, value_parser = ["person", "project"], required = true)]
    scope: String,
    #[command(flatten)]
    content: Content,
    #[command(flatten)]
    words: Words,
}

#[derive(clap::Args)]
pub(crate) struct EditArgs {
    /// The instruction to change.
    id: String,
    /// Where the rule lives: your own rules (person), or this project (project).
    #[arg(long, value_parser = ["person", "project"], required = true)]
    scope: String,
    #[command(flatten)]
    content: Content,
    #[command(flatten)]
    words: Words,
}

#[derive(clap::Args)]
pub(crate) struct RemoveArgs {
    /// The instruction to retire.
    id: String,
    /// Where the rule lives: your own rules (person), or this project (project).
    #[arg(long, value_parser = ["person", "project"], required = true)]
    scope: String,
    #[command(flatten)]
    words: Words,
}

#[derive(clap::Args)]
pub(crate) struct AnswerArgs {
    /// The item the card named.
    r#ref: String,
    /// An option id of that card.
    option: String,
    /// Where the rule lives: your own rules (person), or this project (project).
    #[arg(long, value_parser = ["person", "project"], required = true)]
    scope: String,
    /// The hash of the card the person was shown.
    #[arg(long = "card", value_name = "HASH")]
    card: String,
    /// The person's words, quoted.
    #[arg(long, value_name = "WORDS")]
    evidence: String,
    /// The agent that relays the person's words: your own actor. The run comes from your session.
    #[arg(long, value_name = "ACTOR")]
    relayed_by: String,
    /// The question the person was asked.
    #[arg(long, value_name = "TEXT")]
    question: Option<String>,
}

/// The refusal of Later on the ctl surface, which the CLI is (F4): the words of `rdy answer`.
const NO_LATER: &str = "the CLI has no Later";
const NO_LATER_FIX: &str =
    "answer with another option; Later is a chat act, relayed with rdy_answer";

/// The request in the shape `rdy_rule` takes, and the relayer the command line names.
fn request_of(cmd: RuleCmd) -> (Value, String) {
    fn put_words(v: &mut Value, w: Words) -> String {
        v["why"] = json!(w.why);
        v["evidence"] = json!(w.evidence);
        if let Some(q) = w.question {
            v["question"] = json!(q);
        }
        if let Some(s) = w.session_ref {
            v["session_ref"] = json!(s);
        }
        w.relayed_by
    }
    fn put_content(v: &mut Value, c: Content) {
        v["text"] = json!(c.text);
        if let Some(r) = c.ruleset {
            v["ruleset"] = json!(r);
        }
        if let Some(a) = c.applies_to {
            v["applies_to"] = json!(a);
        }
    }
    match cmd {
        RuleCmd::Add(a) => {
            let mut v = json!({"scope": a.scope, "op": "add"});
            put_content(&mut v, a.content);
            let by = put_words(&mut v, a.words);
            (v, by)
        }
        RuleCmd::Edit(a) => {
            let mut v = json!({"scope": a.scope, "op": "edit", "id": a.id});
            put_content(&mut v, a.content);
            let by = put_words(&mut v, a.words);
            (v, by)
        }
        RuleCmd::Remove(a) => {
            let mut v = json!({"scope": a.scope, "op": "remove", "id": a.id});
            let by = put_words(&mut v, a.words);
            (v, by)
        }
        RuleCmd::Answer(a) => {
            let mut answer = json!({"ref": a.r#ref, "option": a.option, "card_hash": a.card,
                "evidence": a.evidence});
            if let Some(q) = a.question {
                answer["question"] = json!(q);
            }
            (json!({"scope": a.scope, "answer": answer}), a.relayed_by)
        }
    }
}

/// `rdy rule`: the request, checked; then the reply of the engine. `projectless` says that no
/// project was found from the working directory and none was named.
pub(crate) fn rule(
    env: &Env,
    cmd: RuleCmd,
    projectless: bool,
) -> Result<(Value, String), ErrorReply> {
    if let RuleCmd::Answer(a) = &cmd
        && a.option == "later"
    {
        return Err(ErrorReply::new(E_BAD_INPUT, NO_LATER, NO_LATER_FIX));
    }
    let (body, by) = request_of(cmd);
    // the closed form, its bounds and its words: before any session, daemon or store
    let request = parse_request(&body)?;
    let (actor, run) = session_env()?;
    let person = human_actor_for_env(&env.project)?;
    let session = classify(&person, actor.as_deref(), run.as_deref(), Role::Writer)?;
    let words = match &request {
        RuleRequest::Add(r) => &r.relay,
        RuleRequest::Edit(r) => &r.relay,
        RuleRequest::Remove(r) => &r.relay,
        RuleRequest::Answer(r) => &r.relay,
    };
    let input = RelayInput {
        relayed_by: Some(by.clone()),
        evidence: Some(words.evidence.clone()),
        question: words.question.clone(),
        session_ref: words.session_ref.clone(),
        kind: None,
    };
    // the actor pairing and the human's refusal, before any connection
    relayed(&session, &person, &input, false).map_err(|r| ErrorReply::from(r.error()))?;
    let host = person_reader(env.no_daemon);
    let route = if projectless {
        None
    } else {
        Some(rule_route(
            Some(&env.project),
            Some(&host),
            request.scope(),
        )?)
    };
    let v = match route {
        Some(RuleRoute::Here(_)) => {
            let mut args = request.to_json();
            args["relayed_by"] = json!(by);
            call(env, "rule", args)?
        }
        // another project, or none: the person's rule goes straight to the meta project
        Some(RuleRoute::Meta(_)) | None => {
            let agent: Arc<dyn SigningAgent> = rdy_daemon::signing_transport::direct_agent();
            let origin = RuleOrigin {
                session: &session,
                relayed_by: Some(&by),
            };
            route_rule(None, Some(&host), &origin, &request, agent)?
        }
    };
    let t = reply_text(&v);
    for w in v["landing"]["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(v["warnings"].as_array().into_iter().flatten())
    {
        eprintln!(
            "warning {}: {}; {}",
            text(&w["code"]),
            text(&w["message"]),
            text(&w["fix"])
        );
    }
    Ok((v, t))
}

/// Where a reply says the rule lives.
fn destination(v: &Value) -> String {
    if v["scope"] == json!("person") {
        "your own rules".to_string()
    } else {
        format!("project {}", text(&v["project"]["name"]))
    }
}

/// The card text of `entry`: the item's card.
fn card_text(entry: &Value) -> &str {
    text(&entry["card"]["card"])
}

/// The reply as text. A landing says what was saved or removed. A card stop prints each card as it
/// is, the primary one first. A version that is not ready prints its ref and every issue. An
/// answer that did not land prints what was answered, then the cards that wait.
pub(crate) fn reply_text(v: &Value) -> String {
    match v["state"].as_str() {
        Some("card") => {
            let mut cards = vec![card_text(v).to_string()];
            cards.extend(
                v["cards"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| card_text(c).to_string()),
            );
            cards.join("\n\n")
        }
        Some("not_ready") => {
            let mut lines = vec![format!("not ready: {}", text(&v["ref"]))];
            for i in v["issues"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "{}: {} (fix: {})",
                    text(&i["code"]),
                    text(&i["message"]),
                    text(&i["fix"])
                ));
            }
            lines.join("\n")
        }
        Some("answered") => {
            let mut out = format!(
                "answered {}: {}",
                text(&v["ref"]),
                text(&v["answer"]["label"])
            );
            for c in v["remaining_cards"].as_array().into_iter().flatten() {
                out.push_str("\n\n");
                out.push_str(card_text(c));
            }
            out
        }
        _ if v["rule"]["retired"] == json!(true) => format!(
            "Removed rule {} from {}.",
            text(&v["rule"]["id"]),
            destination(v)
        ),
        _ => format!(
            "Saved to {}: \"{}\"",
            destination(v),
            text(&v["rule"]["text"])
        ),
    }
}
