//! D-6M0AF, D-WPE4R, TASK-982 O4 and O5: `rdy_answer {ref, card_hash, elicit: true}` asks the
//! client to put rdy's card to the person. This module holds the parts with no transport in them:
//! the daemon-memory [`Registry`] of waiting cards, the closed input, the form, and the check of
//! what the client returns. `server/intake.rs` runs the two protocol flows on top of them.
//!
//! A waiting card is one [`Bound`] under an opaque handle. The registry belongs to the server's
//! hub, so a second stdio connection of the same actor and run can continue, and it holds no card
//! body, no choice, no signing socket and no store ref. A daemon exit drops every entry.
use crate::claims::Clock;
use crate::tools::AnswerParams;
use rdy_engine::pending::Item;
use rdy_engine::{E_BAD_INPUT, E_ELICITATION_UNAVAILABLE, E_LIMIT, RdyError};
use rmcp::model::{
    ClientCapabilities, ConstTitle, ElicitRequestParams, ElicitationSchema, EnumSchema,
    InputResponses, JsonObject, LegacyEnumSchema, PrimitiveSchemaDefinition, ProtocolVersion,
    SingleSelectEnumSchema, TitledSingleSelectEnumSchema,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The most cards one registry keeps waiting.
pub const MAX_WAITING: usize = 1024;

/// A waiting card is gone at this age.
pub const EXPIRY: Duration = Duration::from_secs(600);

/// The longest item ref an elicited answer binds, in UTF-8 bytes.
pub const MAX_REF_BYTES: usize = 1024;

/// The longest brief token a waiting card keeps, in bytes.
pub const MAX_BRIEF_BYTES: usize = 1024;

/// The one key of `inputRequests` and `inputResponses`.
pub const ANSWER_KEY: &str = "answer";

/// The one field of the form.
const CHOICE: &str = "choice";

/// The keys `elicit: true` refuses when they are supplied, whatever their value.
const NOT_WITH_ELICIT: [&str; 7] = [
    "option",
    "evidence",
    "note",
    "question",
    "session_ref",
    "kind",
    "all_recommended",
];

/// The refusal of a client that cannot show the card.
pub fn unavailable() -> RdyError {
    RdyError::new(
        E_ELICITATION_UNAVAILABLE,
        "this client cannot show rdy's card itself",
        "show the card in chat and relay the person's answer: rdy_answer with option and evidence",
    )
}

/// The refusal of an unknown, expired, used or foreign handle, and of changed arguments.
pub fn gone() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "this elicited answer is no longer available",
        "read rdy_pending, then ask again with rdy_answer and elicit: true",
    )
}

/// The refusal of what a client returned, once the handle was valid.
pub fn invalid_answer() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the client returned an invalid elicited answer",
        "ask again with elicit: true, or show the card in chat and relay the person's answer",
    )
}

fn mixed() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "elicit takes only ref, card_hash and brief",
        "pass ref, card_hash, elicit: true and an optional brief, or relay an answer with option and evidence",
    )
}

/// One waiting card: what the person is asked, and who asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub item: String,
    pub card_hash: String,
    pub brief: Option<String>,
    pub actor: String,
    pub run: Option<String>,
}

struct Entry {
    bound: Bound,
    created: Instant,
}

/// The cards waiting for a person's answer, by handle. In memory only. An entry expires at
/// [`EXPIRY`] and is purged before any insert or lookup.
pub struct Registry {
    clock: Clock,
    entries: Mutex<BTreeMap<String, Entry>>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::with_clock(Arc::new(Instant::now))
    }
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// A registry that reads the time from `clock`.
    pub fn with_clock(clock: Clock) -> Registry {
        Registry {
            clock,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// The entries, with every expired one purged, and the time now.
    fn live(&self) -> (MutexGuard<'_, BTreeMap<String, Entry>>, Instant) {
        let now = (self.clock)();
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.retain(|_, e| now.saturating_duration_since(e.created) < EXPIRY);
        (entries, now)
    }

    /// The number of waiting cards.
    pub fn len(&self) -> usize {
        self.live().0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keeps `bound` waiting, and returns its handle: 16 random bytes as 32 lowercase hex.
    /// `E_LIMIT` when [`MAX_WAITING`] cards wait.
    pub fn insert(&self, bound: Bound) -> Result<String, RdyError> {
        let (mut entries, now) = self.live();
        if entries.len() >= MAX_WAITING {
            return Err(RdyError::new(
                E_LIMIT,
                format!("elicitation exceeds {MAX_WAITING} waiting cards"),
                "ask again later: a waiting card expires after 10 minutes",
            ));
        }
        let handle = loop {
            let handle = new_handle();
            if !entries.contains_key(&handle) {
                break handle;
            }
        };
        entries.insert(
            handle.clone(),
            Entry {
                bound,
                created: now,
            },
        );
        Ok(handle)
    }

    /// Consumes the card under `handle` for the caller and arguments in `asked`, atomically: of
    /// two concurrent retries, one wins. A card of another owner or with other arguments stays
    /// waiting. Every failure is [`gone`].
    pub fn take(&self, handle: &str, asked: &Bound) -> Result<(), RdyError> {
        let (mut entries, _) = self.live();
        match entries.get(handle) {
            Some(e) if e.bound == *asked => {
                entries.remove(handle);
                Ok(())
            }
            _ => Err(gone()),
        }
    }

    /// Drops the card under `handle`, if it still waits.
    pub fn discard(&self, handle: &str) {
        self.live().0.remove(handle);
    }
}

fn new_handle() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `elicit: true` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub item: String,
    pub card_hash: String,
    pub brief: Option<String>,
}

impl Ask {
    /// The waiting card of this ask for `actor` and `run`.
    pub fn bound(self, actor: String, run: Option<String>) -> Bound {
        Bound {
            item: self.item,
            card_hash: self.card_hash,
            brief: self.brief,
            actor,
            run,
        }
    }
}

/// The closed input of `elicit: true`. `raw` is the call's own arguments, where a supplied null
/// is still supplied; the direct tool function has only the parsed parameters. Only `ref`,
/// `card_hash`, `brief` and `elicit` pass, and `ref` and `card_hash` are required.
pub fn ask_of(p: &AnswerParams, raw: Option<&JsonObject>) -> Result<Ask, RdyError> {
    let parsed = [
        p.option.is_some(),
        p.evidence.is_some(),
        p.note.is_some(),
        p.question.is_some(),
        p.session_ref.is_some(),
        p.kind.is_some(),
        p.all_recommended.is_some(),
    ];
    let supplied = NOT_WITH_ELICIT
        .iter()
        .zip(parsed)
        .any(|(key, set)| raw.map_or(set, |args| args.contains_key(*key)));
    if supplied {
        return Err(mixed());
    }
    let (Some(item), Some(card_hash)) = (&p.r#ref, &p.card_hash) else {
        return Err(mixed());
    };
    if item.len() > MAX_REF_BYTES {
        return Err(RdyError::new(
            E_LIMIT,
            format!("the elicited item ref exceeds {MAX_REF_BYTES} bytes"),
            "read rdy_pending and use its item ref",
        ));
    }
    if p.brief.as_ref().is_some_and(|b| b.len() > MAX_BRIEF_BYTES) {
        return Err(RdyError::new(
            E_LIMIT,
            format!("the elicited brief token exceeds {MAX_BRIEF_BYTES} bytes"),
            "pass the token rdy_brief gave",
        ));
    }
    Ok(Ask {
        item: item.clone(),
        card_hash: card_hash.clone(),
        brief: p.brief.clone(),
    })
}

/// How this request can reach the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `input_required`, then a retry of the call (2026-07-28).
    Retry,
    /// `elicitation/create` inside the call (2025-11-25 and 2025-06-18). The older revision's
    /// schema names its options with `enumNames`.
    InCall { enum_names: bool },
    /// No way: an earlier revision, or a client with no form capability.
    Unavailable,
}

/// The route of a request of revision `version` from a client that declared `capabilities`. A
/// client that declares elicitation with no mode means the form (2025-06-18 has no modes); one
/// that declares only the URL mode cannot show a form.
pub fn route(
    version: Option<&ProtocolVersion>,
    capabilities: Option<&ClientCapabilities>,
) -> Route {
    let form = capabilities
        .and_then(|c| c.elicitation.as_ref())
        .is_some_and(|e| e.form.is_some() || e.url.is_none());
    if !form {
        return Route::Unavailable;
    }
    match version {
        Some(v) if *v == ProtocolVersion::V_2026_07_28 => Route::Retry,
        Some(v) if *v == ProtocolVersion::V_2025_11_25 => Route::InCall { enum_names: false },
        Some(v) if *v == ProtocolVersion::V_2025_06_18 => Route::InCall { enum_names: true },
        _ => Route::Unavailable,
    }
}

/// The title of one option: its label, and the mark on a recommended option only.
fn title(o: &rdy_engine::pending::ItemOption) -> String {
    match o.recommended {
        Some(_) => format!("{} (Recommended)", o.label),
        None => o.label.clone(),
    }
}

/// The form of `item`: the engine's card text and one required choice of its option ids, in card
/// order, with no default.
pub fn form(item: &Item, enum_names: bool) -> ElicitRequestParams {
    let select = if enum_names {
        let mut legacy = LegacyEnumSchema::new(item.options.iter().map(|o| o.id.clone()).collect());
        legacy.enum_names = Some(item.options.iter().map(title).collect());
        EnumSchema::Legacy(legacy)
    } else {
        EnumSchema::Single(SingleSelectEnumSchema::Titled(
            TitledSingleSelectEnumSchema::new(
                item.options
                    .iter()
                    .map(|o| ConstTitle::new(o.id.clone(), title(o)))
                    .collect(),
            ),
        ))
    };
    let mut schema = ElicitationSchema::new(BTreeMap::from([(
        CHOICE.to_string(),
        PrimitiveSchemaDefinition::Enum(select),
    )]));
    schema.required = Some(vec![CHOICE.to_string()]);
    ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: item.card.clone(),
        requested_schema: schema,
    }
}

/// What the person did with the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminal {
    Accept(String),
    Decline,
    Cancel,
}

impl Terminal {
    /// The reply of an answer that writes nothing; `None` for a choice.
    pub fn state(&self) -> Option<Value> {
        match self {
            Terminal::Accept(_) => None,
            Terminal::Decline => Some(declined()),
            Terminal::Cancel => Some(cancelled()),
        }
    }
}

/// The reply of a declined form.
pub fn declined() -> Value {
    json!({"state": "declined"})
}

/// The reply of a cancelled form or call.
pub fn cancelled() -> Value {
    json!({"state": "cancelled"})
}

/// An `ElicitResult` object as the client returned it: only `action`, `content` and `_meta`; an
/// accept's content is exactly one string `choice`; a decline's or cancel's content is absent or
/// empty. Anything else is `None`.
pub fn parse_result(v: &Value) -> Option<Terminal> {
    let o = v.as_object()?;
    if o.keys()
        .any(|k| !matches!(k.as_str(), "action" | "content" | "_meta"))
    {
        return None;
    }
    if o.get("_meta").is_some_and(|m| !m.is_object()) {
        return None;
    }
    let content = o.get("content");
    match o.get("action")?.as_str()? {
        "accept" => {
            let c = content?.as_object()?;
            if c.len() != 1 {
                return None;
            }
            Some(Terminal::Accept(c.get(CHOICE)?.as_str()?.to_string()))
        }
        action @ ("decline" | "cancel") => {
            if content.is_some_and(|c| !c.as_object().is_some_and(|c| c.is_empty())) {
                return None;
            }
            Some(if action == "decline" {
                Terminal::Decline
            } else {
                Terminal::Cancel
            })
        }
        _ => None,
    }
}

/// The `inputResponses` of a retry: exactly the [`ANSWER_KEY`] entry, itself an `ElicitResult`.
pub fn parse_responses(responses: Option<&InputResponses>) -> Option<Terminal> {
    let responses = responses?;
    if responses.len() != 1 {
        return None;
    }
    parse_result(responses.get(ANSWER_KEY)?)
}

/// The engine's refusal of a choice that needs a note, with the fix of the elicited path: the form
/// has only the choice. Every other refusal comes back unchanged.
pub fn elicited_refusal(mut e: RdyError) -> RdyError {
    if e.code == E_BAD_INPUT && e.message.contains("needs a note") {
        e.fix = "show the card in chat and relay the answer with a note: rdy_answer with option, note and evidence".to_string();
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui10_results_follow_the_closed_shape() {
        let accept = |choice: &str| Some(Terminal::Accept(choice.to_string()));
        for (v, want) in [
            (
                json!({"action": "accept", "content": {"choice": "o1"}}),
                accept("o1"),
            ),
            (
                json!({"action": "accept", "content": {"choice": "o1"}, "_meta": {"k": 1}}),
                accept("o1"),
            ),
            (json!({"action": "decline"}), Some(Terminal::Decline)),
            (
                json!({"action": "decline", "content": {}}),
                Some(Terminal::Decline),
            ),
            (json!({"action": "cancel"}), Some(Terminal::Cancel)),
            (json!({"action": "accept"}), None),
            (json!({"action": "accept", "content": {}}), None),
            (json!({"action": "accept", "content": {"choice": 1}}), None),
            (
                json!({"action": "accept", "content": {"choice": "o1", "x": 1}}),
                None,
            ),
            (
                json!({"action": "decline", "content": {"choice": "o1"}}),
                None,
            ),
            (json!({"action": "decline", "content": null}), None),
            (json!({"action": "cancel", "extra": 1}), None),
            (json!({"action": "cancel", "_meta": 1}), None),
            (json!({"action": "maybe"}), None),
            (json!({"content": {"choice": "o1"}}), None),
            (json!("accept"), None),
        ] {
            assert_eq!(parse_result(&v), want, "{v}");
        }
    }

    #[test]
    fn ui10_a_handle_is_32_lowercase_hex() {
        let h = new_handle();
        assert_eq!(h.len(), 32);
        assert!(h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }
}
