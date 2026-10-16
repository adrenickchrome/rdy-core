//! The relay table (D-HA7YZ, D-BGYNM, D-KQ4CA): who may record a person's act, and how. Every
//! relayed human act goes through [`relayed`]: an answer, an approval at the daemon, and the
//! relayed tag, dismissal and view acts that follow it.
//!
//! # The table (closed)
//!
//! A relay is the person's words (`evidence`), the question they answered (`question`) and the
//! chat session they spoke in (`session_ref`), recorded by the agent that relays them.
//!
//! 1. A human session with no relay input: the person acts as themselves. No relay.
//! 2. A human session that names `relayed_by` or `evidence`: refused. A person answers as
//!    themselves; only the relaying agent records a relay ([`RelayRefusal::HumanRelays`]).
//! 3. An agent session with no `evidence` (or only blanks): refused. An agent acts on a human
//!    act only by relaying the person's words ([`RelayRefusal::NoEvidence`]).
//! 4. An agent session with `evidence`. Over MCP (`implicit`), the session is the relay:
//!    `relayed_by` must be absent ([`RelayRefusal::RelayedByOverMcp`]). At the command line,
//!    `relayed_by` must be given ([`RelayRefusal::NoRelayedBy`]) and must be the session's actor,
//!    or `<actor>/<run>` when the session has a run ([`RelayRefusal::NotItself`]). The flag is
//!    compared whole and never split, since an actor may hold `/`. The relay records the session's
//!    own actor and run.
//! 5. `question` or `session_ref` without `evidence`: refused
//!    ([`RelayRefusal::NeedsEvidence`]).
//! 6. The quote and the question hold 1 to 2000 characters after trimming
//!    ([`RelayRefusal::TooLong`]); `session_ref` holds at most a view name's length
//!    ([`RelayRefusal::SessionRefTooLong`]) and defaults to `mcp` (implicit) or `cli`.
//!
//! In cases 1 and 4 the act runs as `Session::human(person, Role::Approver)`: the person is the
//! actor of the act, and the agent is `relayed_by`.
//!
//! # Wording
//!
//! [`relayed`] returns a closed [`RelayRefusal`], and each caller words it (O19).
//! [`RelayRefusal::error`] gives the answer's texts; the daemon's `relay_approver` keeps its own.
use crate::Session;
use crate::error::{E_BAD_INPUT, E_PERMISSION, RdyError};
use rdy_model::{ActorRun, EvidenceKind, RelayEvidence, Role};
use rdy_store::Relay;

/// The most characters a quote or a question holds, after trimming.
pub const MAX_QUOTE: usize = 2000;

/// The most characters a `session_ref` holds, as a view name.
const MAX_SESSION_REF: usize = crate::write::MAX_VIEW_NAME;

/// What a caller passes for a relay. Every field is optional; [`relayed`] checks them together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelayInput {
    /// The relaying agent as the command line names it: `<actor>` or `<actor>/<run>`. Absent over
    /// MCP, where the session is the relay.
    pub relayed_by: Option<String>,
    /// The person's words, quoted.
    pub evidence: Option<String>,
    /// The question the person answered.
    pub question: Option<String>,
    /// The chat session the person spoke in.
    pub session_ref: Option<String>,
    /// How the relay got the answer: `card` or `text`.
    pub kind: Option<EvidenceKind>,
}

/// Why [`relayed`] refuses: cases 2 to 6 of the table, closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayRefusal {
    /// 2: a human session names a relay.
    HumanRelays,
    /// 3: an agent session gives no evidence.
    NoEvidence,
    /// 4: an agent over MCP names `relayed_by`.
    RelayedByOverMcp,
    /// 4: an agent at the command line names no `relayed_by`.
    NoRelayedBy,
    /// 4: `relayed_by` (`by`) is not the session's own actor or actor/run.
    NotItself { actor: String, by: String },
    /// 5: `field` (`question` or `session_ref`) without evidence.
    NeedsEvidence { field: &'static str },
    /// 6: the quote or the question is longer than [`MAX_QUOTE`].
    TooLong,
    /// 6: `session_ref` is longer than a view name.
    SessionRefTooLong,
}

impl RelayRefusal {
    /// The refusal in the answer's words (plan Decision intake, Task 7).
    pub fn error(&self) -> RdyError {
        match self {
            RelayRefusal::HumanRelays => RdyError::new(
                E_PERMISSION,
                "a person answers as themselves; only the relaying agent records a relay",
                "drop the relay flags, or let the agent answer with its own RDY_ACTOR",
            ),
            RelayRefusal::NoEvidence => RdyError::new(
                E_PERMISSION,
                "an agent answers only by relaying a person's words",
                "ask the person, then pass their words as evidence and your question",
            ),
            RelayRefusal::RelayedByOverMcp => RdyError::new(
                E_BAD_INPUT,
                "relayed_by names the agent relaying at the command line; over MCP the session is the relay",
                "drop relayed_by",
            ),
            RelayRefusal::NoRelayedBy => RdyError::new(
                E_BAD_INPUT,
                "relayed_by and evidence go together",
                "pass both (--relayed-by and --evidence) or neither",
            ),
            RelayRefusal::NotItself { actor, by } => RdyError::new(
                E_PERMISSION,
                format!("{actor} relays only as itself, not as {by}"),
                format!("pass --relayed-by {actor} with the person's words as evidence"),
            ),
            RelayRefusal::NeedsEvidence { field } => RdyError::new(
                E_BAD_INPUT,
                format!("{field} needs evidence"),
                format!("pass evidence with it, or drop {field}"),
            ),
            RelayRefusal::TooLong => RdyError::new(
                E_BAD_INPUT,
                format!("evidence and question take at most {MAX_QUOTE} characters each"),
                "quote the person's answer, not the whole conversation",
            ),
            RelayRefusal::SessionRefTooLong => RdyError::new(
                E_BAD_INPUT,
                format!("session_ref takes at most {MAX_SESSION_REF} characters"),
                "use a short session id, not the conversation",
            ),
        }
    }
}

/// The session an act runs as, and the relay it records: the table in the module docs.
/// `person` is the project's human actor, from the caller (O18). `implicit` is true over MCP,
/// where the session is the relay.
pub fn relayed(
    s: &Session,
    person: &str,
    r: &RelayInput,
    implicit: bool,
) -> Result<(Session, Option<Relay>), RelayRefusal> {
    let as_person = || Session::human(person.to_string(), Role::Approver);
    if s.is_human() {
        // 2
        if r.relayed_by.is_some() || r.evidence.is_some() {
            return Err(RelayRefusal::HumanRelays);
        }
        // 5
        needs_evidence(r)?;
        // 1
        return Ok((as_person(), None));
    }
    // 3
    let quote = r
        .evidence
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or(RelayRefusal::NoEvidence)?;
    // 4
    if implicit {
        if r.relayed_by.is_some() {
            return Err(RelayRefusal::RelayedByOverMcp);
        }
    } else {
        let by = r.relayed_by.as_deref().ok_or(RelayRefusal::NoRelayedBy)?;
        let me = match &s.run {
            Some(run) => format!("{}/{run}", s.actor),
            None => s.actor.clone(),
        };
        if by != s.actor && by != me {
            return Err(RelayRefusal::NotItself {
                actor: s.actor.clone(),
                by: by.to_string(),
            });
        }
    }
    // 6
    let question = r
        .question
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    if [Some(quote), question]
        .iter()
        .flatten()
        .any(|t| t.chars().count() > MAX_QUOTE)
    {
        return Err(RelayRefusal::TooLong);
    }
    let session_ref = r
        .session_ref
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    if session_ref.is_some_and(|q| q.chars().count() > MAX_SESSION_REF) {
        return Err(RelayRefusal::SessionRefTooLong);
    }
    let session_ref = session_ref.unwrap_or(if implicit { "mcp" } else { "cli" });
    Ok((
        as_person(),
        Some(Relay {
            by: ActorRun {
                actor: s.actor.clone(),
                run: s.run.clone(),
            },
            evidence: RelayEvidence {
                quoted: quote.to_string(),
                session_ref: session_ref.to_string(),
                question: question.map(str::to_string),
            },
        }),
    ))
}

/// Case 5: `question` and `session_ref` come only with evidence.
fn needs_evidence(r: &RelayInput) -> Result<(), RelayRefusal> {
    if r.evidence.is_some() {
        return Ok(());
    }
    for (field, given) in [
        ("question", r.question.is_some()),
        ("session_ref", r.session_ref.is_some()),
    ] {
        if given {
            return Err(RelayRefusal::NeedsEvidence { field });
        }
    }
    Ok(())
}
