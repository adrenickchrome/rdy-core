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
//! The ref is an item ref of the closed grammar of O3 ([`is_item_ref`]), or `E_BAD_INPUT`. The
//! item must be in the person's queue now, read with no session and before the cap of
//! [`crate::pending::queue::PENDING_ITEMS_MAX`] items, so neither a snooze nor the cap hides it
//! from an answer (final review fix round 1, R2: a card a snooze brings into the first page is
//! answered as shown).
//! - A proposal ref that is no longer its proposal's latest version: `E_PROPOSAL_MOVED`, with the
//!   text of an approval of a moved version.
//! - An approval (option `approve`, or the recommended option of a list) of a proposal's latest
//!   version that has not landed, was not withdrawn and is not ready, in any spelling of its ref:
//!   `E_PROPOSAL_NOT_READY`, with the queue's readiness issues in the queue's order and no card,
//!   as every approval path refuses it (D-ME2HD).
//! - Any other ref that is not in the queue (its version moved, it was answered, it landed, or
//!   its batch is not ready and the answer is no approval): `E_ITEM_MOVED`, `<ref> changed since
//!   it was shown`. A batch that is not ready lists its issues. One exception: a `defer:P-x@n`
//!   ref of an open proposal's latest version binds its deferral card whether or not the version
//!   is ready or the card answered (D-AAQ9Z), so a refused card takes a later acceptance; every
//!   other check below applies.
//! - A relayed answer passes `card_hash`. A local answer may omit it, and then binds to the
//!   version only. A decision version proposed in two views stands beside each batch with the
//!   same ref and its own card, so a `card_hash` picks the card of that ref with that hash. When
//!   no card of the ref has the hash given: `E_PROPOSAL_MOVED` for an `approve` item,
//!   `E_ITEM_MOVED` for the others. For an `approve` item the landing's freshness check runs
//!   first (D-WQVGT, RULING F1): its `E_PROPOSAL_MOVED` passes through with its `corrected`
//!   (the fresh card, `{"card": ...}`, or the fresh readiness issues and no card).
//! - The option is one of the item's option ids, or `E_BAD_INPUT`.
//!
//! Each refusal of a moved item carries the item's fresh card, when the queue has one, in
//! `corrected` as `{"ref", "card_hash", "card"}` (O21).
//!
//! # Acts
//!
//! The batch acts, and Later on any item, are in [`batch`]. An approval lands through `land_with`
//! and writes no `ANSWER`; every other batch act writes one `ANSWER` record in a batch of its
//! own. The answer to a question is in [`question`]: a proposed decision, then its `ANSWER`;
//! only it reads `brief`, which its write cites under stable content (bwr1). The
//! answers to tensions are in [`resolve`]: an acceptance writes semantic tension versions and no
//! `ANSWER`; a dismissal writes its reviews and its `ANSWER` in one batch. The answers to a
//! re-affirmation card are in [`reaffirm`]: `Keep them suspect` writes one `ANSWER`; `They still
//! hold` lands the card's members alone, in an isolated landing that records the answer
//! (D-CQMA4).
pub mod batch;
pub mod question;
pub mod reaffirm;
pub mod relay;
pub mod resolve;

use crate::error::{E_BAD_INPUT, E_ITEM_MOVED, E_PROPOSAL_MOVED, RdyError};
use crate::gate::{
    LandInfo, landed_as, latest_version, moved, proposal_not_ready, resolve_in, withdrawn,
    withdrawn_error,
};
use crate::pending::PendingQuery;
use crate::pending::queue::{Eligible, deferral_card};
use crate::pending::reaffirm::REAFFIRM_REF_PREFIX;
use crate::pending::{CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, Item, ItemKind, ItemOption};
use crate::proposal_show::is_proposal_ref;
use crate::tensions::set::tension_hex;
use crate::{Rdy, ReadState, Session};
use rdy_model::{ActorRun, BlockId, EvidenceKind, Hash};
use rdy_store::{Relay, StoreError};
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
    /// The briefing the answer's write cites (a `brief:` token from rdy_brief): an answer to a
    /// question whose decision goes under stable content needs one, as any write there does.
    /// Every other act writes no content and ignores it.
    pub brief: Option<String>,
}

/// What an answer did.
#[derive(Clone, Debug, Serialize)]
pub struct AnswerInfo {
    pub r#ref: String,
    /// `approve` for a landing, `accept` for an acceptance of advice, else the act of the
    /// `ANSWER` record (`reject_proposal`, `later`, `confirm`, `reject_decision`,
    /// `answer_question`, `dismiss`).
    pub act: String,
    pub option: String,
    /// The chosen option's label, from rdy's own card; for a free answer (`o0`), the note.
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

/// What the first half of an answer did: an act other than an approval, written; or an
/// approval, prepared under the writer and not yet landed ([`Rdy::commit_answer`] lands it).
pub(crate) enum AnswerStep {
    Done(Box<AnswerInfo>),
    Approval(Box<PreparedAnswer>),
}

/// An approval answer prepared under the writer: the landing to commit and the answer's reply
/// before its `landing` and `next` are known.
pub(crate) struct PreparedAnswer {
    pub(crate) landing: crate::gate::PreparedApproval,
    pub(crate) info: AnswerInfo,
    /// The chat session whose queue gives `next` after the commit: `Some` for a single answer
    /// (the relay's `session_ref`, or none), `None` for an answer of a list, whose caller sets
    /// `next` on the last one.
    pub(crate) next_session: Option<Option<String>>,
}

/// The session an answer runs as, its relay and its evidence kind, the session that gave the
/// answer (the relaying agent's, or the person's own) and the briefing its write cites (only an
/// answer to a question reads it; a batch of recommended answers cites none).
#[derive(Clone)]
pub(crate) struct Answerer {
    pub who: Session,
    pub relay: Option<Relay>,
    pub kind: Option<EvidenceKind>,
    pub from: Session,
    pub brief: Option<String>,
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
        match self.answer_step(s, person, req)? {
            AnswerStep::Done(info) => Ok(*info),
            AnswerStep::Approval(prepared) => self.commit_answer(*prepared, &[], person),
        }
    }

    /// `answer` up to its write: every act but an approval is written (with its `next`); an
    /// approval is prepared and not landed.
    pub(crate) fn answer_step(
        &mut self,
        s: &Session,
        person: &str,
        req: AnswerRequest,
    ) -> Result<AnswerStep, RdyError> {
        let by = Answerer {
            brief: req.brief,
            ..answerer(s, person, &req.relay)?
        };
        // a re-affirmation card checks its own note: a reason for each option (D-CQMA4)
        let note = if req.r#ref.starts_with(REAFFIRM_REF_PREFIX) {
            req.note
        } else {
            note_of(req.note)?
        };
        let step = self.answer_one(
            &by,
            person,
            &req.r#ref,
            Pick::Id(&req.option),
            req.card_hash.as_deref(),
            note,
        )?;
        Ok(match step {
            AnswerStep::Done(mut info) => {
                info.next = next_of(&self.read(), person, &by);
                AnswerStep::Done(info)
            }
            AnswerStep::Approval(mut prepared) => {
                prepared.next_session = Some(session_ref_of(&by));
                AnswerStep::Approval(prepared)
            }
        })
    }

    /// Lands the approval `prepared` holds, with `signatures` of its landing in the same batch,
    /// and completes its reply: the landing, and `next` for a single answer.
    pub(crate) fn commit_answer(
        &mut self,
        prepared: PreparedAnswer,
        signatures: &[rdy_model::signature::Signature],
        person: &str,
    ) -> Result<AnswerInfo, RdyError> {
        let PreparedAnswer {
            landing,
            mut info,
            next_session,
        } = prepared;
        info.landing = Some(self.commit_landing(landing, signatures)?);
        if let Some(session_ref) = next_session {
            info.next = next_in(&self.read(), person, session_ref);
        }
        Ok(info)
    }

    /// Answers each of `refs` with its recommended option, in the given order, each bound to its
    /// own card hash (`card_hashes`, one a ref; none for a local answer) and with the same relay.
    /// Each answer is its own record (p-E975A). It stops at the first refusal: the error's
    /// `corrected` then lists, under `done`, the answers given before it.
    ///
    /// Content removal is never approved in a batch (D-CC90E: "never under a policy"). Before it
    /// answers anything, the call resolves every proposal ref of `refs` to the version it names,
    /// whether or not that version is ready, latest or in the queue, and reads its changes: if
    /// any carries a removal request, the whole call is `E_REDACTION_HUMAN` and nothing is
    /// answered or landed, wherever the ref stands in the list. The refusal never depends on the
    /// card: `corrected` holds the queue's card of that proposal only when one renders within
    /// [`CARD_MAX_TOKENS`].
    pub fn answer_all_recommended(
        &mut self,
        s: &Session,
        person: &str,
        refs: &[String],
        card_hashes: &[String],
        relay: RelayInput,
    ) -> Result<Vec<AnswerInfo>, RdyError> {
        let by = self.recommended_answerer(s, person, refs, card_hashes, &relay)?;
        let mut done: Vec<AnswerInfo> = vec![];
        for (n, r) in refs.iter().enumerate() {
            let shown = card_hashes.get(n).map(String::as_str);
            let one = self
                .recommended_step(&by, person, r, shown)
                .and_then(|step| match step {
                    AnswerStep::Done(info) => Ok(*info),
                    AnswerStep::Approval(prepared) => self.commit_answer(*prepared, &[], person),
                });
            match one {
                Ok(info) => done.push(info),
                Err(e) => return Err(with_done(e, &done)),
            }
        }
        if let Some(last) = done.last_mut() {
            last.next = next_of(&self.read(), person, &by);
        }
        Ok(done)
    }

    /// The checks `answer_all_recommended` makes before it answers anything: one card hash for
    /// each ref (or none), the relay table, the removal pre-scan (D-CC90E: a list that holds a
    /// removal is `E_REDACTION_HUMAN` as a whole) and the deferral pre-scan (D-AAQ9Z: a deferral
    /// card has no recommended option, so a list that names one is `E_BAD_INPUT` as a whole).
    /// Returns the answerer of every item.
    pub(crate) fn recommended_answerer(
        &self,
        s: &Session,
        person: &str,
        refs: &[String],
        card_hashes: &[String],
        relay: &RelayInput,
    ) -> Result<Answerer, RdyError> {
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
        let by = answerer(s, person, relay)?;
        let st = self.read();
        if let Some(r) = removal_ref(&st, refs)? {
            let e = RdyError::from(StoreError::RedactionHuman);
            return Err(match removal_card(&st, person, r) {
                Some(item) => e.with_corrected(card_of(&item)),
                None => e,
            });
        }
        // D-AAQ9Z, D-CQMA4: a deferral card and a re-affirmation card have no recommended
        // option, so a list that names one is refused as a whole, before anything is answered
        if let Some(r) = refs.iter().find(|r| {
            (r.starts_with("defer:") || r.starts_with(REAFFIRM_REF_PREFIX)) && is_item_ref(r)
        }) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{r} has no recommended option"),
                "answer it on its own",
            ));
        }
        Ok(by)
    }

    /// One item of a list of recommended answers, up to its write: an approval is prepared,
    /// every other act written. `next` is left to the caller.
    pub(crate) fn recommended_step(
        &mut self,
        by: &Answerer,
        person: &str,
        r: &str,
        shown: Option<&str>,
    ) -> Result<AnswerStep, RdyError> {
        self.answer_one(by, person, r, Pick::Recommended, shown, None)
    }

    fn answer_one(
        &mut self,
        by: &Answerer,
        person: &str,
        r: &str,
        pick: Pick<'_>,
        shown: Option<&str>,
        note: Option<String>,
    ) -> Result<AnswerStep, RdyError> {
        let bound = match bind(&self.read(), person, r, shown, by.relay.is_some(), pick)? {
            Binding::Bound(b) => b,
            // RULING F1: the person saw another card of the approval; when the landing's
            // freshness check fails, its refusal (with the fresh card) passes through
            Binding::ShownMoved(e) => {
                return Err(match self.check_fresh(r) {
                    Err(f) if f.code == E_PROPOSAL_MOVED => f,
                    _ => e,
                });
            }
        };
        match (bound.item.kind, bound.option.id.as_str()) {
            (_, "later") | (ItemKind::Approve | ItemKind::Confirm | ItemKind::Defer, _) => {
                batch::act(self, &by.who, by.relay.clone(), by.kind, bound, note)
            }
            (ItemKind::Answer, _) => {
                question::act(self, by, person, bound, note).map(|i| AnswerStep::Done(Box::new(i)))
            }
            (ItemKind::Resolve, _) => {
                resolve::act(self, by, bound, note).map(|i| AnswerStep::Done(Box::new(i)))
            }
            (ItemKind::Reaffirm, _) => reaffirm::act(self, by, bound, note),
        }
    }
}

/// The first of `refs` that names a proposal version carrying a removal request (D-CC90E). It
/// reads only, and resolves each proposal ref itself, so neither readiness nor a later version
/// hides a removal request. A ref that does not resolve is left to its own answer, in its turn.
fn removal_ref<'a>(st: &ReadState, refs: &'a [String]) -> Result<Option<&'a str>, RdyError> {
    let reader = st.reader();
    for r in refs {
        if !(r.contains('@') && is_proposal_ref(r)) {
            continue;
        }
        let Ok(pref) = resolve_in(st, r) else {
            continue;
        };
        for h in &reader.proposal(&pref.version)?.changes {
            if reader.redaction_for_carrier(h)?.is_some() {
                return Ok(Some(r));
            }
        }
    }
    Ok(None)
}

/// The queue's card of the proposal `r` names (its own version, else the version that stands
/// there now), when it renders within [`CARD_MAX_TOKENS`]. It only decorates a refusal, so any
/// error gives none.
fn removal_card(st: &ReadState, person: &str, r: &str) -> Option<Item> {
    let q = st
        .eligible(&PendingQuery {
            person: person.to_string(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .ok()?;
    match q.items_of(r).ok()?.into_iter().next() {
        Some(item) => Some(item),
        None => fresh(&q, r).ok().flatten(),
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
    Ok(Answerer {
        who,
        relay,
        kind,
        from: s.clone(),
        brief: None,
    })
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

/// What [`bind`] found for an item ref.
enum Binding {
    Bound(Bound),
    /// An approve card of the ref stands, but none with the hash shown: the refusal it gets
    /// when the landing's freshness check passes (`E_PROPOSAL_MOVED`, with the queue's card).
    ShownMoved(RdyError),
}

/// The item `r` of `person`'s queue, bound (see the module docs).
fn bind(
    st: &ReadState,
    person: &str,
    r: &str,
    shown: Option<&str>,
    relayed: bool,
    pick: Pick<'_>,
) -> Result<Binding, RdyError> {
    if !is_item_ref(r) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{r} is not an item ref"),
            "pass an item ref as rdy_pending gives it: P-<id>@<n>, <D-id>@<version>, <Q-id>@<version>, advice:P-<id>@<n> or a tension id",
        ));
    }
    let q = st.eligible(&PendingQuery {
        person: person.to_string(),
        session_ref: None,
        max_tokens: CARD_MAX_TOKENS,
    })?;
    let mut cards = q.items_of(r)?;
    // D-AAQ9Z: an explicit deferral ref of the current version binds its card outside the ready
    // queue too, so a refused card takes a later answer (the latest answer wins)
    if cards.is_empty()
        && let Some(card) = deferral_card(st, r)?
    {
        cards.push(card);
    }
    let Some(first) = cards.first() else {
        let approving = matches!(pick, Pick::Id("approve") | Pick::Recommended);
        return Err(gone(st, &q, r, approving)?);
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
    // the card of ref r with the hash shown: a decision version beside two batches has two
    let item = match card_hash.map(|h| h.to_string()) {
        None => first,
        Some(h) => match cards.iter().find(|i| i.card_hash == h) {
            Some(item) => item,
            None if first.kind == ItemKind::Approve => {
                return Ok(Binding::ShownMoved(
                    changed(E_PROPOSAL_MOVED, r).with_corrected(card_of(first)),
                ));
            }
            None => return Err(changed(E_ITEM_MOVED, r).with_corrected(card_of(first))),
        },
    };
    let option = match pick {
        // a free answer to a question: the person's own words, in the note
        Pick::Id(id) if id == question::FREE && item.kind == ItemKind::Answer => question::free(),
        Pick::Id(id) => item
            .options
            .iter()
            .find(|o| o.id == id)
            .ok_or_else(|| {
                let ids: Vec<&str> = item.options.iter().map(|o| o.id.as_str()).collect();
                RdyError::new(
                    E_BAD_INPUT,
                    format!("{id} is not an option of {r}"),
                    format!("answer with one of: {}", ids.join(", ")),
                )
            })?
            .clone(),
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
            })?
            .clone(),
    };
    Ok(Binding::Bound(Bound {
        item: item.clone(),
        option,
        card_hash,
    }))
}

/// Whether `r` is an item ref (O3, closed): `P-<id>@<n>` (an approval), `<block id>@<version
/// short>` (a decision's confirm or a question's answer: the 12 lowercase hex digits of the
/// block's version), `advice:P-<id>@<n>` (a version's advised tensions), `defer:P-<id>@<n>` (a
/// version's deferral card, an explicit version only), `reaffirm:<block id>@<version short>` (a
/// re-affirmation card: its cause and the first 12 lowercase hex digits of the cause's version)
/// or a tension id (`tn-` and 6 to 64 hex digits, or the hex alone). The refs of relayed acts
/// that are not items (`tag:`, `own:`, `stale:`, `propose:`) are not item refs.
pub fn is_item_ref(r: &str) -> bool {
    let proposal_version = |p: &str| p.contains('@') && is_proposal_ref(p);
    let block_version = |p: &str| {
        p.split_once('@').is_some_and(|(id, v)| {
            BlockId::parse(id).is_ok()
                && v.len() == 12
                && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    };
    if let Some(p) = r.strip_prefix("defer:") {
        return proposal_version(p);
    }
    if let Some(p) = r.strip_prefix(REAFFIRM_REF_PREFIX) {
        return block_version(p);
    }
    match r.strip_prefix("advice:") {
        Some(p) => proposal_version(p),
        None => proposal_version(r) || block_version(r) || tension_hex(r).is_some(),
    }
}

/// The refusal of `r`, which is not in the queue `q`: a proposal version that is not its
/// latest is `E_PROPOSAL_MOVED`; a latest version that has not landed and that a close of its
/// view withdrew is `E_VIEW_CLOSED` (D-K0SS1), with no fresh card (the queue has none); an
/// approval (`approving`) of a latest version that has not landed and that the queue lists as
/// not ready, by its canonical ref, is `E_PROPOSAL_NOT_READY` with the queue's issues and no
/// card (D-ME2HD); anything else, an `advice:` ref of a withdrawn version too (R3), a rejection
/// or a Later of a version that is not ready, and a version a `reject_proposal` answer took out,
/// is `E_ITEM_MOVED`. `E_PROPOSAL_MOVED` and `E_ITEM_MOVED` carry the fresh card when the queue
/// has one, and a batch that is not ready lists its issues.
fn gone(st: &ReadState, q: &Eligible, r: &str, approving: bool) -> Result<RdyError, RdyError> {
    let mut e = None;
    if is_proposal_ref(r) {
        let pref = resolve_in(st, r)?;
        let latest = latest_version(st, &pref.stable)?;
        let p = st.reader().proposal(&pref.version)?;
        if latest != pref.version {
            e = Some(moved(st, &pref, &latest, &p.view)?);
        } else if landed_as(st, &p)?.is_none() {
            if withdrawn(st, &pref.version, &p)? {
                return Ok(withdrawn_error(&pref, &p.view));
            }
            let canonical = pref.to_string();
            if approving && let Some(n) = q.not_ready.iter().find(|n| n.r#ref == canonical) {
                return Ok(proposal_not_ready(&pref, n.issues.clone()));
            }
        }
    }
    let mut e = e.unwrap_or_else(|| changed(E_ITEM_MOVED, r));
    if let Some(n) = q.not_ready.iter().find(|n| n.r#ref == r) {
        e.issues.clone_from(&n.issues);
    }
    Ok(match fresh(q, r)? {
        Some(i) => e.with_corrected(card_of(&i)),
        None => e,
    })
}

/// The item of the queue that stands where `r` stood: the same proposal, decision or question,
/// at another version (the ref up to its `@`).
fn fresh(q: &Eligible, r: &str) -> Result<Option<Item>, RdyError> {
    let key = match r.rfind('@') {
        Some(at) => r[..=at].to_string(),
        None => format!("{r}@"),
    };
    q.first_with_prefix(&key)
}

fn changed(code: &'static str, r: &str) -> RdyError {
    RdyError::new(
        code,
        format!("{r} changed since it was shown"),
        "show the person the fresh card and ask again",
    )
}

/// The labels the card shows, in order.
pub(crate) fn labels(i: &Item) -> Vec<String> {
    i.options.iter().map(|o| o.label.clone()).collect()
}

/// `local`, or `relayed` with a relay.
pub(crate) fn assurance(relay: &Option<Relay>) -> &'static str {
    if relay.is_some() { "relayed" } else { "local" }
}

/// Writes the answer `a` in a batch of its own and publishes it (`Delta::Seq`, O14).
pub(crate) fn put_alone(r: &mut Rdy, a: &rdy_model::Answer) -> Result<Hash, RdyError> {
    // a refusal means rdy built an answer the store's rules reject: a bug, not bad input
    let h = r.store_mut().put_answer(a).map_err(internal)?;
    r.publish(crate::Delta::Seq)?;
    Ok(h)
}

/// A store error as the answer reports it: a refusal is a bug in rdy.
pub(crate) fn internal(e: rdy_store::StoreError) -> RdyError {
    match e {
        rdy_store::StoreError::Refused(message) => RdyError::new(
            crate::error::E_INTERNAL,
            message,
            "report this as a bug in rdy",
        ),
        e => e.into(),
    }
}

/// An item's card as a refusal carries it (O21).
pub(crate) fn card_of(i: &Item) -> Value {
    json!({"ref": i.r#ref, "card_hash": i.card_hash, "card": i.card})
}

/// `e`, with the answers given before it under `done` in `corrected`.
pub(crate) fn with_done(mut e: RdyError, done: &[AnswerInfo]) -> RdyError {
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
pub(crate) fn next_of(st: &ReadState, person: &str, by: &Answerer) -> Option<String> {
    next_in(st, person, session_ref_of(by))
}

/// The chat session of `by`'s relay, if any.
fn session_ref_of(by: &Answerer) -> Option<String> {
    by.relay.as_ref().map(|r| r.evidence.session_ref.clone())
}

/// The first item of `person`'s queue as chat session `session_ref` sees it.
fn next_in(st: &ReadState, person: &str, session_ref: Option<String>) -> Option<String> {
    let q = st
        .pending(&PendingQuery {
            person: person.to_string(),
            session_ref,
            max_tokens: CARD_DEFAULT_TOKENS,
        })
        .ok()?;
    q.items.first().map(|i| i.r#ref.clone())
}
