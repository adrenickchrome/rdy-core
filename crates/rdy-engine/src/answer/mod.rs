//! The answer (D-JV6Z8, D-ST423): one op for every item of the "needs you" queue, local or
//! relayed, bound to the version and the card the person was shown (F6).
//!
//! # The relay
//!
//! Every answer goes through the relay table ([`relay::relayed`], D-HA7YZ). A human session
//! answers as itself (`local`). An agent session answers only by relaying the person's words
//! (`relayed`). An agent session that names no `relayed_by` relays over MCP, where the session is
//! the relay; at the command line clap pairs `--relayed-by` with `--evidence`, so half a relay
//! never reaches the engine (O19). A relay's evidence is `card` (the default) or `text`;
//! `elicited` waits for M3 (F5).
//!
//! # Binding (closed)
//!
//! The item must be in the person's queue now, read with no session, so a snooze does not hide
//! it from an answer.
//! - A proposal ref that is no longer its proposal's latest version: `E_PROPOSAL_MOVED`, with the
//!   text of an approval of a moved version.
//! - Any other ref that is not in the queue (its version moved, it was answered, it landed, or
//!   its batch is not ready): `E_ITEM_MOVED`, `<ref> changed since it was shown`. A batch that is
//!   not ready lists its issues.
//! - A relayed answer passes `card_hash`. A local answer may omit it, and then binds to the
//!   version only. When the card's hash is not the hash given: `E_PROPOSAL_MOVED` for an
//!   `approve` item, `E_ITEM_MOVED` for the others.
//! - The option is one of the item's option ids, or `E_BAD_INPUT`.
//!
//! Each refusal of a moved item carries the item's fresh card, when the queue has one, in
//! `corrected` as `{"ref", "card_hash", "card"}` (O21).
//!
//! # Acts
//!
//! The batch acts are in [`batch`]. An approval lands through `land_with` and writes no
//! `ANSWER`; every other act writes one `ANSWER` record in a batch of its own.
pub mod batch;
pub mod relay;

use crate::error::{E_BAD_INPUT, E_ITEM_MOVED, E_PROPOSAL_MOVED, RdyError};
use crate::gate::{LandInfo, latest_version, moved, resolve_in};
use crate::pending::PendingQuery;
use crate::pending::{CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, Item, ItemKind, ItemOption, Pending};
use crate::proposal_show::is_proposal_ref;
use crate::{Rdy, ReadState, Session};
use rdy_model::{ActorRun, EvidenceKind, Hash};
use rdy_store::Relay;
use relay::{MAX_QUOTE, RelayInput, relayed};
use serde::Serialize;
use serde_json::{Value, json};

/// One answer: the item's ref, the option id chosen, the card the person was shown, the
/// person's note and the relay.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnswerRequest {
    /// The item ref, as the queue gives it (`P-x@n`, `D-x@v`, ...).
    pub r#ref: String,
    /// The id of the option chosen (`approve`, `reject`, `later`, `confirm`, ...).
    pub option: String,
    /// The `card_hash` of the card the person was shown. A relayed answer passes it.
    pub card_hash: Option<String>,
    /// The person's note, 1 to 2000 characters.
    pub note: Option<String>,
    pub relay: RelayInput,
}

/// What an answer did.
#[derive(Clone, Debug, Serialize)]
pub struct AnswerInfo {
    pub r#ref: String,
    /// `approve` for a landing, else the act of the `ANSWER` record (`reject_proposal`, `later`,
    /// `confirm`, `reject_decision`).
    pub act: String,
    pub option: String,
    /// The chosen option's label, from rdy's own card.
    pub label: String,
    /// `local`, or `relayed` when an agent relayed the person's words.
    pub assurance: &'static str,
    /// The `ANSWER` record's hash, when the act wrote one.
    pub answer: Option<String>,
    /// The landing, for an approval.
    pub landing: Option<LandInfo>,
    pub relayed_by: Option<ActorRun>,
    /// The ref of the first item left in the person's queue, if any.
    pub next: Option<String>,
}

/// An item of the queue, bound for an answer: the item as the queue shows it now, the option
/// chosen and the card hash the person was shown.
pub(crate) struct Bound {
    pub item: Item,
    pub option: ItemOption,
    pub card_hash: Option<Hash>,
}

/// Which option an answer chooses.
enum Pick<'a> {
    /// The option of this id.
    Id(&'a str),
    /// The item's recommended option.
    Recommended,
}

/// The session an answer runs as, its relay and its evidence kind.
struct Answerer {
    who: Session,
    relay: Option<Relay>,
    kind: Option<EvidenceKind>,
}

impl Rdy {
    /// Answers one item of `person`'s queue (see the module docs). `person` is the project's
    /// human actor, from the caller (O18).
    pub fn answer(
        &mut self,
        s: &Session,
        person: &str,
        req: AnswerRequest,
    ) -> Result<AnswerInfo, RdyError> {
        let by = answerer(s, person, &req.relay)?;
        let note = note_of(req.note)?;
        let mut info = self.answer_one(
            &by,
            person,
            &req.r#ref,
            Pick::Id(&req.option),
            req.card_hash.as_deref(),
            note,
        )?;
        info.next = next_of(&self.read(), person, &by);
        Ok(info)
    }

    /// Answers each of `refs` with its recommended option, in the given order, each bound to its
    /// own card hash (`card_hashes`, one a ref; none for a local answer) and with the same relay.
    /// Each answer is its own record (p-E975A). It stops at the first refusal: the error's
    /// `corrected` then lists, under `done`, the answers given before it.
    pub fn answer_all_recommended(
        &mut self,
        s: &Session,
        person: &str,
        refs: &[String],
        card_hashes: &[String],
        relay: RelayInput,
    ) -> Result<Vec<AnswerInfo>, RdyError> {
        if !card_hashes.is_empty() && card_hashes.len() != refs.len() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "{} refs and {} card hashes: give one card hash for each ref",
                    refs.len(),
                    card_hashes.len()
                ),
                "pass the card_hash of each card, in the order of the refs",
            ));
        }
        let by = answerer(s, person, &relay)?;
        let mut done: Vec<AnswerInfo> = vec![];
        for (n, r) in refs.iter().enumerate() {
            let shown = card_hashes.get(n).map(String::as_str);
            match self.answer_one(&by, person, r, Pick::Recommended, shown, None) {
                Ok(info) => done.push(info),
                Err(e) => return Err(with_done(e, &done)),
            }
        }
        if let Some(last) = done.last_mut() {
            last.next = next_of(&self.read(), person, &by);
        }
        Ok(done)
    }

    fn answer_one(
        &mut self,
        by: &Answerer,
        person: &str,
        r: &str,
        pick: Pick<'_>,
        shown: Option<&str>,
        note: Option<String>,
    ) -> Result<AnswerInfo, RdyError> {
        let bound = bind(&self.read(), person, r, shown, by.relay.is_some(), pick)?;
        batch::act(self, &by.who, by.relay.clone(), by.kind, bound, note)
    }
}

/// The relay table for `s`, and the evidence kind a relay records.
fn answerer(s: &Session, person: &str, r: &RelayInput) -> Result<Answerer, RdyError> {
    // an agent that names no relayed_by relays over MCP: the session is the relay
    let implicit = !s.is_human() && r.relayed_by.is_none();
    let (who, relay) = relayed(s, person, r, implicit).map_err(|e| e.error())?;
    let kind = match (&relay, r.kind) {
        (None, _) => None,
        (Some(_), None | Some(EvidenceKind::Card)) => Some(EvidenceKind::Card),
        (Some(_), Some(EvidenceKind::Text)) => Some(EvidenceKind::Text),
        (Some(_), Some(EvidenceKind::Elicited)) => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a relayed answer's evidence is card or text",
                "pass kind card for an answer on the card, or text for the person's own words",
            ));
        }
    };
    Ok(Answerer { who, relay, kind })
}

/// The note, trimmed: none when blank, refused past [`MAX_QUOTE`] characters.
fn note_of(note: Option<String>) -> Result<Option<String>, RdyError> {
    let Some(note) = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if note.chars().count() > MAX_QUOTE {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("a note takes at most {MAX_QUOTE} characters"),
            "keep the person's note short",
        ));
    }
    Ok(Some(note))
}

/// The item `r` of `person`'s queue, bound (see the module docs).
fn bind(
    st: &ReadState,
    person: &str,
    r: &str,
    shown: Option<&str>,
    relayed: bool,
    pick: Pick<'_>,
) -> Result<Bound, RdyError> {
    let q = st.pending(&PendingQuery {
        person: person.to_string(),
        session_ref: None,
        max_tokens: CARD_MAX_TOKENS,
    })?;
    let Some(item) = q.items.iter().find(|i| i.r#ref == r) else {
        return Err(gone(st, &q, r)?);
    };
    let card_hash = match shown {
        Some(h) => Some(h.parse::<Hash>().map_err(|_| {
            RdyError::new(
                E_BAD_INPUT,
                format!("card_hash {h} is not a hash"),
                "pass the card_hash rdy_pending gave with the card",
            )
        })?),
        None if relayed => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "a relayed answer names the card the person was shown",
                "pass the card_hash rdy_pending gave with the card",
            ));
        }
        None => None,
    };
    if card_hash.is_some_and(|h| h.to_string() != item.card_hash) {
        let code = if item.kind == ItemKind::Approve {
            E_PROPOSAL_MOVED
        } else {
            E_ITEM_MOVED
        };
        return Err(changed(code, r).with_corrected(card_of(item)));
    }
    let option = match pick {
        Pick::Id(id) => item.options.iter().find(|o| o.id == id).ok_or_else(|| {
            let ids: Vec<&str> = item.options.iter().map(|o| o.id.as_str()).collect();
            RdyError::new(
                E_BAD_INPUT,
                format!("{id} is not an option of {r}"),
                format!("answer with one of: {}", ids.join(", ")),
            )
        })?,
        Pick::Recommended => item
            .options
            .iter()
            .find(|o| o.recommended.is_some())
            .ok_or_else(|| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("{r} has no recommended option"),
                    "answer it on its own",
                )
            })?,
    };
    Ok(Bound {
        item: item.clone(),
        option: option.clone(),
        card_hash,
    })
}

/// The refusal of `r`, which is not in the queue `q`: a proposal version that is not its
/// latest is `E_PROPOSAL_MOVED`; anything else is `E_ITEM_MOVED`. Either carries the fresh card
/// when the queue has one, and a batch that is not ready lists its issues.
fn gone(st: &ReadState, q: &Pending, r: &str) -> Result<RdyError, RdyError> {
    let mut e = None;
    if is_proposal_ref(r) {
        let pref = resolve_in(st, r)?;
        let latest = latest_version(st, &pref.stable)?;
        if latest != pref.version {
            let p = st.reader().proposal(&pref.version)?;
            e = Some(moved(st, &pref, &latest, &p.view)?);
        }
    }
    let mut e = e.unwrap_or_else(|| changed(E_ITEM_MOVED, r));
    if let Some(n) = q.not_ready.iter().find(|n| n.r#ref == r) {
        e.issues.clone_from(&n.issues);
    }
    Ok(match fresh(q, r) {
        Some(i) => e.with_corrected(card_of(i)),
        None => e,
    })
}

/// The item of the queue that stands where `r` stood: the same proposal, decision or question,
/// at another version (the ref up to its `@`).
fn fresh<'a>(q: &'a Pending, r: &str) -> Option<&'a Item> {
    let key = match r.rfind('@') {
        Some(at) => r[..=at].to_string(),
        None => format!("{r}@"),
    };
    q.items.iter().find(|i| i.r#ref.starts_with(&key))
}

fn changed(code: &'static str, r: &str) -> RdyError {
    RdyError::new(
        code,
        format!("{r} changed since it was shown"),
        "show the person the fresh card and ask again",
    )
}

/// An item's card as a refusal carries it (O21).
pub(crate) fn card_of(i: &Item) -> Value {
    json!({"ref": i.r#ref, "card_hash": i.card_hash, "card": i.card})
}

/// `e`, with the answers given before it under `done` in `corrected`.
fn with_done(mut e: RdyError, done: &[AnswerInfo]) -> RdyError {
    let done = serde_json::to_value(done).unwrap_or(Value::Null);
    match &mut e.corrected {
        Some(Value::Object(m)) => {
            m.insert("done".into(), done);
        }
        _ => e.corrected = Some(json!({ "done": done })),
    }
    e
}

/// The first item of `person`'s queue after an answer, as the relay's chat session sees it.
fn next_of(st: &ReadState, person: &str, by: &Answerer) -> Option<String> {
    let q = st
        .pending(&PendingQuery {
            person: person.to_string(),
            session_ref: by.relay.as_ref().map(|r| r.evidence.session_ref.clone()),
            max_tokens: CARD_DEFAULT_TOKENS,
        })
        .ok()?;
    q.items.first().map(|i| i.r#ref.clone())
}
