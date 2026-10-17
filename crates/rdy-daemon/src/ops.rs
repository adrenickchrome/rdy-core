//! The ctl ops: one function per op, each against `&dyn Engine`. Every read op takes one
//! snapshot (`engine.read()`) and reads only through it; none touches the writer. Every write
//! op parses and checks its role here, then runs its engine call as one `Engine::write`
//! closure on the writer. Relative paths in args resolve against the session's working
//! directory.
use rdy_engine::answer::relay::{RelayInput, RelayRefusal, relayed};
use rdy_engine::engine::Engine;
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::tensions::api::{TensionsRequest, run_resolve, run_tensions};
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_PERMISSION, RdyError, ReadState, Relay, Scope, Session, alt, intent,
    view_api,
};
use rdy_mcp::tools::MAX_TOKENS_LIMIT;
use rdy_model::Role;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

mod brief;
mod clone;
mod dump;
mod edit;
mod intake;
mod land;
mod project;
mod read;
mod redaction;
mod search;
mod site;
mod site_url;
mod wc;

use brief::brief;
use clone::clone;
use dump::dump;
use edit::{edit_block, keys_assign, new_doc, new_folder};
use intake::{answer, pending};
use land::{approve, propose, tag};
pub use project::{Cleaned, clean_quarantine, quarantine};
use project::{check, fsck, reindex};
use read::{block, coverage, impact, log, ls, show, status, suspects, tags};
use redaction::redact;
use search::search;
pub(crate) use site::export_print;
use site::{EXPORT_SHAPE, export, import, render};
use site_url::site_url;
pub use wc::wc_in_flight;
use wc::{wc_commit, wc_nonce, wc_put_blob, wc_view};

/// What an op runs against: the engine, the calling session, the project's human actor (the
/// one the session was classified against) and the session's working directory.
pub struct OpContext<'a> {
    pub engine: &'a dyn Engine,
    pub session: &'a Session,
    /// The human actor: a relayed approval is recorded as this person's (D-HA7YZ).
    pub human: &'a str,
    pub cwd: &'a Path,
}

/// Every ctl op, in the order the docs list them. `session` is answered by the connection
/// host.
pub const OPS: &[&str] = &[
    "session",
    "status",
    "show",
    "ls",
    "log",
    "impact",
    "suspects",
    "tags",
    "block",
    "brief",
    "search",
    "render",
    "export",
    "dump",
    "clone",
    "redact",
    "check",
    "new_folder",
    "new_doc",
    "keys_assign",
    "edit_block",
    "propose",
    "approve",
    "tag",
    "import",
    "reindex",
    "fsck",
    "view",
    "wc_view",
    "wc_commit",
    "wc_nonce",
    "intent",
    "alt",
    "tensions",
    "resolve",
    "wc_put_blob",
    "coverage",
    "inbox",
    "site_url",
    "pending",
    "answer",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// Why an agent session is refused a tag or an import.
pub(crate) const AGENT_NOT_APPROVER: &str = "agent sessions cannot tag or import";

/// How the refusal is fixed.
pub(crate) const AGENT_NOT_APPROVER_FIX: &str =
    "tags and imports are the person's acts: ask the person to run the command";

/// Why an agent session is refused an approval it does not relay.
const AGENT_NOT_RELAYING: &str = "an agent approves only by relaying a person's approval";

/// How that refusal is fixed.
const AGENT_NOT_RELAYING_FIX: &str = "ask the person, then pass --relayed-by <your actor> --evidence \"<their words>\" --question \"<what you asked>\"";

/// Why a human session is refused a relay (D-HA7YZ, Q2).
const HUMAN_RELAY: &str =
    "a person approves as themselves; only the relaying agent records a relay";

/// How that refusal is fixed.
const HUMAN_RELAY_FIX: &str = "drop --relayed-by to approve yourself, or let the agent run rdy approve with RDY_ACTOR set to its actor";

/// The session a tag or an import runs as: a human session as an approver; an agent session
/// is refused with `E_PERMISSION`.
pub(crate) fn approver(s: &Session) -> Result<Session, RdyError> {
    if s.is_human() {
        Ok(Session {
            role: Role::Approver,
            ..s.clone()
        })
    } else {
        Err(RdyError::new(
            E_PERMISSION,
            AGENT_NOT_APPROVER,
            AGENT_NOT_APPROVER_FIX,
        ))
    }
}

/// The session an approval runs as, and the agent that relays it (D-HA7YZ). It runs the relay
/// table (`answer::relay::relayed`, at the command line: not implicit) and words its refusals
/// in the approval's own texts (O19):
/// - a human session with no relay flags: the human, as an approver; no relay;
/// - a human session with `relayed_by`: refused (`E_PERMISSION`); with evidence alone: refused
///   (`E_BAD_INPUT`, the pairing);
/// - an agent session with no `relayed_by`: refused (`E_PERMISSION`);
/// - an agent session with `relayed_by` but no evidence: refused (`E_BAD_INPUT`, the pairing);
///   with blank evidence, the landing's own refusal of an empty quote (`E_BAD_INPUT`);
/// - an agent session whose `relayed_by` is exactly its actor, or its `actor/run` when it has a
///   run: the human actor (`cx.human`) as the approver, relayed by the session's actor and run
///   (taken from the session, never parsed from the flag);
/// - an agent session whose `relayed_by` is anything else: refused (`E_PERMISSION`), with a fix
///   that names the session's actor, a spelling this check accepts;
/// - a human session with `session_ref` or `question` and no relay: refused (`E_BAD_INPUT`,
///   `<field> needs relayed_by`), `session_ref` named first;
/// - a quote or a question longer than the table allows, or a `session_ref` longer than a view
///   name: the table's refusal (`E_BAD_INPUT`).
///
/// The whole relay (`relayed_by`, `evidence`, `question`, `session_ref`) goes through the table.
/// The relayer it returns is the one the landing records.
pub(crate) fn relay_approver(
    cx: &OpContext,
    input: &RelayInput,
) -> Result<(Session, Option<Relay>), RdyError> {
    let s = cx.session;
    match relayed(s, cx.human, input, false) {
        // a human approves as the session's own actor, as before the table
        Ok((_, None)) => Ok((approver(s)?, None)),
        Ok((who, Some(relay))) => Ok((who, Some(relay))),
        Err(refusal) => Err(approval_refusal(s, input, refusal)),
    }
}

/// A refusal of the relay table in the approval's own texts and codes.
fn approval_refusal(s: &Session, input: &RelayInput, refusal: RelayRefusal) -> RdyError {
    let relayed_by = input.relayed_by.as_deref();
    let evidence = input.evidence.as_deref();
    let pair = || RdyError::new(E_BAD_INPUT, RELAY_PAIR, RELAY_PAIR_FIX);
    let not_relaying = || RdyError::new(E_PERMISSION, AGENT_NOT_RELAYING, AGENT_NOT_RELAYING_FIX);
    match refusal {
        RelayRefusal::HumanRelays if relayed_by.is_some() => {
            RdyError::new(E_PERMISSION, HUMAN_RELAY, HUMAN_RELAY_FIX)
        }
        RelayRefusal::HumanRelays => pair(),
        RelayRefusal::NoEvidence => match (relayed_by, evidence) {
            (None, _) => not_relaying(),
            (Some(_), None) => pair(),
            // the flag names the relayer before the quote is read
            (Some(by), Some(_)) => match not_itself(s, by) {
                Some(e) => e,
                None => RdyError::new(
                    E_BAD_INPUT,
                    "the evidence quote is empty",
                    "quote the human's words with --evidence",
                ),
            },
        },
        RelayRefusal::NoRelayedBy | RelayRefusal::RelayedByOverMcp => not_relaying(),
        RelayRefusal::NotItself { by, .. } => not_itself(s, &by).unwrap_or_else(pair),
        // a person's session_ref or question with no relay: named in the approval's order
        RelayRefusal::NeedsEvidence { .. } if relayed_by.is_none() && evidence.is_none() => {
            let field = if input.session_ref.is_some() {
                "session_ref"
            } else {
                "question"
            };
            RdyError::new(
                E_BAD_INPUT,
                format!("{field} needs relayed_by"),
                format!("pass relayed_by and evidence with it, or drop {field}"),
            )
        }
        RelayRefusal::NeedsEvidence { .. } => pair(),
        r @ (RelayRefusal::TooLong | RelayRefusal::SessionRefTooLong) => r.error(),
    }
}

/// The refusal of a `relayed_by` that is not the session's actor or `actor/run`, if it is not.
/// The flag is compared whole with the session's own spellings, never split: an agent actor may
/// contain '/', so splitting would misread `agent:a/b` as actor `agent:a`, run `b`.
fn not_itself(s: &Session, by: &str) -> Option<RdyError> {
    let me = match &s.run {
        Some(r) => format!("{}/{r}", s.actor),
        None => s.actor.clone(),
    };
    (by != s.actor && by != me).then(|| {
        RdyError::new(
            E_PERMISSION,
            format!("{me} relays only as itself, not as {by}"),
            format!(
                "pass --relayed-by {} with the person's words in --evidence",
                s.actor
            ),
        )
    })
}

/// Why half a relay is refused.
pub(crate) const RELAY_PAIR: &str = "relayed_by and evidence go together";

/// How that refusal is fixed.
pub(crate) const RELAY_PAIR_FIX: &str = "pass both (--relayed-by and --evidence) or neither";

/// The view a write op targets: `view` without a leading `view:`, else the session's default.
pub(crate) fn view_of(s: &Session, view: Option<String>) -> String {
    match view {
        Some(v) => v.strip_prefix("view:").map(String::from).unwrap_or(v),
        None => s.default_view(),
    }
}

/// Deserializes `args` as `T`; a failure is `E_BAD_INPUT` naming the op and its shape (an
/// unknown field by its path in `args`).
fn parse<T: serde::de::DeserializeOwned>(
    op: &str,
    args: Value,
    shape: &str,
) -> Result<T, RdyError> {
    let args = if args.is_null() { json!({}) } else { args };
    T::deserialize(&args).map_err(|e| {
        let msg = e.to_string();
        rdy_engine::did_you_mean::unknown_field_error_in(op, &msg, &args).unwrap_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{op}: {msg}"),
                format!("args for {op}: {shape}"),
            )
        })
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("a reply serializes")
}

/// `p` as given when absolute, else under `cwd`.
pub(crate) fn resolve_path(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// The scope a read op names; reads default to canon, as the CLI does.
pub(crate) fn scope_of(
    st: &ReadState,
    s: &OpContext,
    text: Option<&str>,
) -> Result<Scope, RdyError> {
    match text {
        Some(t) => st.resolve_read_scope(t, Some(&s.session.default_view())),
        None => Ok(Scope::Canon),
    }
}

/// Runs the ctl op `op` with `args`. An unknown op is `E_BAD_INPUT` naming every op.
///
/// Every op is a new response, so it first checks the state the engine serves now: a state
/// read before a content removal refuses with `E_REDACTED_STATE`, before any op parses or reads
/// (plan Redaction: one response-check layer, no name allowlist). The CLI's direct route calls
/// this function too. The acquired state marks where the response starts; an op may read a
/// newer one.
pub fn dispatch(cx: &OpContext, op: &str, args: Value) -> Result<Value, RdyError> {
    let state = cx.engine.read();
    state.ensure_current_epoch()?;
    drop(state);
    match op {
        "session" => Err(RdyError::new(
            E_BAD_INPUT,
            "the session op is answered by the daemon connection, not by dispatch",
            "send it once, first, on a ctl connection",
        )),
        "status" => {
            parse::<NoArgs>(op, args, "{}")?;
            status(cx)
        }
        "show" => show(cx, parse(op, args, "{id, scope?, blame?}")?),
        "ls" => ls(cx, parse(op, args, "{path?, scope?, depth?}")?),
        "log" => log(cx, parse(op, args, "{limit?, since?}")?),
        "impact" => impact(cx, parse(op, args, "{id, depth?, scope?}")?),
        "suspects" => suspects(cx, parse(op, args, "{scope?}")?),
        "tags" => {
            parse::<NoArgs>(op, args, "{}")?;
            tags(cx)
        }
        "block" => block(cx, parse(op, args, "{id, view?}")?),
        "brief" => brief(
            cx,
            parse(
                op,
                args,
                "{ids?, path?, code_path?, page?, scope?, view?, max_tokens?}",
            )?,
        ),
        "search" => search(
            cx,
            parse(
                op,
                args,
                "{query, regex?, ignore_case?, scope?, max_tokens?, page?, types?, traits?, path?, lifecycle?, changed_since?, exclude_history?, governed_by?, describes?, linked_to?, mentions?, suspect?, tension?, coverage_below?, sort?}",
            )?,
        ),
        "render" => render(cx, parse(op, args, "{out, scope?}")?),
        "export" => export(cx, parse(op, args, EXPORT_SHAPE)?),
        "dump" => dump(cx, parse(op, args, "{out}")?),
        "clone" => clone(cx, parse(op, args, "{dir, fork?}")?),
        "redact" => redact(
            cx,
            parse(
                op,
                args,
                "{targets, reason_class, reason, dry_run?, view?, max_tokens?}",
            )?,
        ),
        "check" => check(cx, parse(op, args, "{path, pack?, wc?, view?}")?),
        "new_folder" => new_folder(cx, parse(op, args, "{path, view?}")?),
        "new_doc" => new_doc(cx, parse(op, args, "{path, view?, doc_type?, title?}")?),
        "keys_assign" => keys_assign(cx, parse(op, args, "{view?}")?),
        "edit_block" => edit_block(
            cx,
            parse(
                op,
                args,
                "{yaml, view?, base?, conflict?, brief?, max_tokens?, reason?}",
            )?,
        ),
        "propose" => propose(
            cx,
            parse(
                op,
                args,
                "{title, view?, summary?, max_tokens?, alternatives?, relayed_by?, evidence?, question?, session_ref?}",
            )?,
        ),
        "approve" => approve(
            cx,
            parse(
                op,
                args,
                "{proposal, relayed_by?, evidence?, question?, session_ref?, strict?}",
            )?,
        ),
        "tag" => tag(
            cx,
            parse(
                op,
                args,
                "{version, landing?, message?, relayed_by?, evidence?, question?, session_ref?}",
            )?,
        ),
        "import" => import(cx, parse(op, args, "{dir, view}")?),
        "reindex" => {
            parse::<NoArgs>(op, args, "{}")?;
            reindex(cx)
        }
        "fsck" => fsck(cx, parse(op, args, "{clean_quarantine?}")?),
        "view" => view_api::run_view(
            cx.engine,
            cx.session,
            cx.human,
            &cx.session.default_view(),
            parse(
                op,
                args,
                "{op, name?, from?, base?, pin?, view?, views?, changes?, cascade?, a?, b?, all?, stale?, reason?, owner?, none?, target?, max_tokens?, relayed_by?, evidence?, question?, session_ref?}",
            )?,
        ),
        "wc_view" => wc_view(cx, parse(op, args, "{view?, ids?, blobs_dir?}")?),
        "wc_commit" => wc_commit(
            cx,
            parse(
                op,
                args,
                "{view, ops, nonce, rewrite, message?, implements?, editorial?, brief?, max_tokens?}",
            )?,
        ),
        "wc_nonce" => wc_nonce(
            cx,
            parse(op, args, "{view, nonce, rewrite, recent?, since?}")?,
        ),
        "intent" => intent::run_intent(
            cx.engine,
            cx.session,
            parse(
                op,
                args,
                "{op, view?, area?, blocks?, angle?, ttl?, intent?, block?, actor?, include_expired?}",
            )?,
        ),
        "alt" => alt::run_alt(
            cx.engine,
            cx.session,
            parse(
                op,
                args,
                "{op, title?, question?, members?, set?, label?, view?, member?, reason?, all?}",
            )?,
        ),
        "tensions" => {
            let req: TensionsRequest = parse(
                op,
                args,
                "{scope?, kind?, severity?, status?, view?, area?, involving?, reviews?, max_tokens?}",
            )?;
            check_max_tokens(req.max_tokens)?;
            run_tensions(cx.engine, cx.session, req)
        }
        "resolve" => run_resolve(
            cx.engine,
            cx.session,
            cx.human,
            parse(
                op,
                args,
                "{tension?, mention?, dismiss?, restore?, scope?, accept?}",
            )?,
        ),
        "wc_put_blob" => wc_put_blob(cx, parse(op, args, "{path}")?),
        "coverage" => coverage(cx, parse(op, args, "{scope?, path?, max_tokens?}")?),
        "inbox" => {
            let req: InboxRequest = parse(op, args, "{since?, ack?, max_tokens?}")?;
            check_max_tokens(req.max_tokens)?;
            run_inbox(cx.engine, cx.session, req)
        }
        "site_url" => site_url(cx, parse(op, args, "{scope?}")?),
        "pending" => pending(cx, parse(op, args, "{max_tokens?, session_ref?}")?),
        "answer" => answer(
            cx,
            parse(
                op,
                args,
                "{ref?, option?, card_hash?, note?, brief?, relayed_by?, evidence?, question?, session_ref?, kind?, all_recommended?}",
            )?,
        ),
        _ => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown op {op:?}"),
            format!("one of: {}", OPS.join(", ")),
        )),
    }
}

/// `E_LIMIT` for a `max_tokens` over `MAX_TOKENS_LIMIT`, as the MCP tools refuse it.
fn check_max_tokens(max_tokens: Option<usize>) -> Result<(), RdyError> {
    match max_tokens {
        Some(n) if n > MAX_TOKENS_LIMIT => Err(RdyError::new(
            E_LIMIT,
            format!("max_tokens {n} is over the limit of {MAX_TOKENS_LIMIT}"),
            format!("ask for at most {MAX_TOKENS_LIMIT} tokens"),
        )),
        _ => Ok(()),
    }
}
