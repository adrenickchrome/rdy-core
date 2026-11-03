//! The handlers of the local approval pages (D-6M0AF, D-WPE4R, D-N507H, D-X61DT; TASK-982 I1, O3,
//! O7, O9, O11): the person's pending page, one item's page, and the answer form.
//!
//! `server::serve_conn` routes a request here through [`ui_request::classify`](super::ui_request)
//! before ordinary routing, and runs each handler on the blocking pool. A page reads the engine's
//! complete queue for the configured person and renders it in memory; nothing here touches a
//! site target, a build or a stream. An answer goes through the answer engine as that person's
//! local session, evidence `card`, with no agent: the engine binds the role, the hash and the
//! option inside its write path. A briefing is fetched only for an item's `GET`, never during an
//! answer.
//!
//! Every engine refusal keeps its code, message and fix on a rendered page ([`status_of`] gives
//! its status). A moved card comes back as the fresh one: a card that is not a question with its
//! new form, a question with a link to a new `GET`. Before any page goes out, the epoch of the
//! state it read is checked again, and a state a removal made old gets the existing `Busy`.
use crate::shared::Shared;
use crate::site::guard::{Refusal, refusal};
use crate::site::http::{Response, content_type, ui_response};
use crate::site::live::LiveSite;
use crate::site::ui_request::Form;
use rdy_engine::answer::{
    Presentation, PresentedAnswer, answered_in_view, presented_item, presented_question_brief,
    run_presented_answer,
};
use rdy_engine::config_levels::human_actor_for_env;
use rdy_engine::pending::{CARD_MAX_TOKENS, PendingQuery};
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_engine::{
    E_BAD_INPUT, E_INTERNAL, E_IO, E_ITEM_MOVED, E_LIMIT, E_NO_AGENT, E_NOT_FOUND, E_PERMISSION,
    E_PROPOSAL_MOVED, E_REDACTED_STATE, E_RESERVED_VIEW, E_UNKNOWN_REF, RdyError, ReadState,
    Session, serve_config,
};
use rdy_model::Role;
use rdy_render::approver::{UiPage, item_page, pending_page, refusal_page};
use std::sync::Arc;

/// What a page handler found.
pub(crate) enum Outcome {
    /// The response to send.
    Done(Response),
    /// The item is not there: the listener's own not-found page.
    Missing,
}

/// The status of an engine refusal on a rendered page (TASK-982 O7, closed): a moved card, a role
/// or signing refusal, bad input, an internal failure, and every other code as its readiness or
/// hold refusal.
fn status_of(code: &str) -> u16 {
    match code {
        E_PROPOSAL_MOVED | E_ITEM_MOVED => 409,
        E_PERMISSION | E_NO_AGENT => 403,
        E_BAD_INPUT | E_UNKNOWN_REF | E_NOT_FOUND | E_RESERVED_VIEW | E_LIMIT => 400,
        E_IO | E_INTERNAL => 500,
        _ => 422,
    }
}

/// Whether the UI is on for the next request (D-6M0AF: read fresh for each one). A changed config
/// is reloaded first, whose publish reloads the open UI streams; with `serve.ui` off, they close.
pub(crate) fn enabled(sh: &Shared) -> bool {
    if sh.watch.changed() {
        let _ = sh.writer.reload();
    }
    let on = serve_config(&sh.root).ui;
    if !on {
        sh.site.close_ui();
    }
    on
}

/// The events URL of a page that read `st`.
fn events(st: &ReadState) -> String {
    format!("/_rdy/events?ui=1&rev={}", LiveSite::ui_revision(st))
}

/// The configured person, and their local session.
fn person(sh: &Shared) -> Result<(String, Session), RdyError> {
    let person = human_actor_for_env(&sh.root)?;
    let session = Session::human(person.clone(), Role::Approver)?;
    Ok((person, session))
}

/// `page` with `status`, once `st`'s epoch is checked again; a page that did not render is the
/// existing plain 500, never another rendered page.
fn sent(st: &ReadState, status: u16, page: Result<UiPage, RdyError>) -> Response {
    match page {
        Ok(_) if st.ensure_current_epoch().is_err() => refusal(Refusal::Busy),
        Ok(p) => ui_response(status, content_type("page.html"), p.html),
        Err(e) if e.code == E_REDACTED_STATE => refusal(Refusal::Busy),
        Err(_) => refusal(Refusal::Broken),
    }
}

/// The page of engine refusal `e`, from the state published now. A moved card's `corrected` names
/// its ref and its fresh hash: that card is read again by that ref and hash, and shown with its
/// form (a question with none). If it moved or refuses again, or another refusal carries a card,
/// the page shows the refusal and the card's escaped text. With no card, the refusal alone.
fn refused(sh: &Shared, person: Option<&str>, e: &RdyError) -> Response {
    if e.code == E_REDACTED_STATE {
        return refusal(Refusal::Busy);
    }
    let st = sh.state.load_full();
    let fresh = match (person, e.code, e.corrected.as_ref()) {
        (Some(person), E_PROPOSAL_MOVED | E_ITEM_MOVED, Some(c)) => {
            match (c["ref"].as_str(), c["card_hash"].as_str()) {
                (Some(r), Some(h)) => presented_item(&st, person, r, Some(h)).ok(),
                _ => None,
            }
        }
        _ => None,
    };
    let page = match &fresh {
        Some(item) => item_page(&st, item, None, Some(e), None, None),
        None => refusal_page(&st, e),
    };
    sent(&st, status_of(e.code), page)
}

/// `GET` and `HEAD /~ui/`: the configured person's queue, each card whole, with the reload stream.
pub(crate) fn pending(sh: &Shared) -> Response {
    let (person, _) = match person(sh) {
        Ok(p) => p,
        Err(e) => return refused(sh, None, &e),
    };
    pending_with(sh, &person, None, true)
}

/// The pending page of `person`, with `notice`, and the stream when `live`. A page answering a
/// `POST` has none: its reload would post the form again.
fn pending_with(sh: &Shared, person: &str, notice: Option<&str>, live: bool) -> Response {
    let st = sh.state.load_full();
    let queue = st.pending(&PendingQuery {
        person: person.to_string(),
        session_ref: None,
        max_tokens: CARD_MAX_TOKENS,
    });
    match queue {
        Ok(q) => {
            let url = live.then(|| events(&st));
            let answered: Result<Vec<_>, RdyError> = q
                .items
                .iter()
                .filter_map(|i| answered_in_view(&st, person, i).transpose())
                .collect();
            let answered = match answered {
                Ok(a) => a,
                Err(e) => return refused(sh, Some(person), &e),
            };
            let page = pending_page(&st, &q, notice, url.as_deref(), &answered);
            sent(&st, 200, page)
        }
        Err(e) => refused(sh, Some(person), &e),
    }
}

/// `GET` and `HEAD /~ui/item/<ref>`: the current card of `reference` (O11), for a question with
/// the briefing of its decision's destination (TASK-982 §1), and the reload stream. A ref that is
/// no item of the queue is [`Outcome::Missing`]; a moved ref shows its fresh card; a role refusal
/// is its own page.
pub(crate) fn item(sh: &Shared, reference: &str) -> Outcome {
    let (person, session) = match person(sh) {
        Ok(p) => p,
        Err(e) => return Outcome::Done(refused(sh, None, &e)),
    };
    let st = sh.state.load_full();
    let found = match presented_item(&st, &person, reference, None) {
        Ok(found) => found,
        Err(e) if absent(&e) => return Outcome::Missing,
        Err(e) => return Outcome::Done(refused(sh, Some(&person), &e)),
    };
    let briefing = match presented_question_brief(&st, &sh.briefs, &session, &found) {
        Ok(b) => b,
        Err(e) => return Outcome::Done(refused(sh, Some(&person), &e)),
    };
    let answered = match answered_in_view(&st, &person, &found) {
        Ok(a) => a,
        Err(e) => return Outcome::Done(refused(sh, Some(&person), &e)),
    };
    let url = events(&st);
    let page = item_page(
        &st,
        &found,
        briefing.as_ref(),
        None,
        Some(&url),
        answered.as_ref(),
    );
    Outcome::Done(sent(&st, 200, page))
}

/// Whether a lookup's refusal says the queue has no such item: a ref outside the grammar, one that
/// resolves to nothing, or an item gone with no fresh card to show.
fn absent(e: &RdyError) -> bool {
    match e.code {
        E_BAD_INPUT | E_UNKNOWN_REF | E_NOT_FOUND => true,
        E_ITEM_MOVED => e.corrected.is_none(),
        _ => false,
    }
}

/// `POST /~ui/answer`: the form's answer through the answer engine (see the module docs). A
/// success is `303` to the pending page; a UI Later is that page with its notice, and no write.
pub(crate) fn answer(sh: &Arc<Shared>, form: Form) -> Response {
    let (person, session) = match person(sh) {
        Ok(p) => p,
        Err(e) => return refused(sh, None, &e),
    };
    let reference = form.reference.clone();
    let reply = run_presented_answer(
        &*sh.engine(),
        &UnavailableAgent,
        &session,
        &person,
        Presentation::Ui,
        PresentedAnswer {
            reference: form.reference,
            card_hash: form.card_hash,
            option: form.option,
            note: form.note,
            brief: form.brief,
        },
    );
    match reply {
        Ok(v) if v["state"] == "later" => {
            let notice =
                format!("Later keeps {reference} open. It stays on this page until you answer it.");
            pending_with(sh, &person, Some(&notice), false)
        }
        Ok(_) => {
            let mut done = ui_response(303, content_type("page.html"), Vec::new());
            done.headers.push(("Location", "/~ui/".to_string()));
            done
        }
        Err(e) => refused(sh, Some(&person), &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O7's closed table, and its exhaustive fallback for every other code.
    #[test]
    fn ui9_every_refusal_code_has_its_status() {
        for (code, status) in [
            (E_PROPOSAL_MOVED, 409),
            (E_ITEM_MOVED, 409),
            (E_PERMISSION, 403),
            (E_NO_AGENT, 403),
            (E_BAD_INPUT, 400),
            (E_UNKNOWN_REF, 400),
            (E_NOT_FOUND, 400),
            (E_RESERVED_VIEW, 400),
            (E_LIMIT, 400),
            (E_IO, 500),
            (E_INTERNAL, 500),
            (rdy_engine::E_BRIEF_REQUIRED, 422),
            (rdy_engine::E_BRIEF_STALE, 422),
            (rdy_engine::E_EXCEPTION_OPEN, 422),
            (rdy_engine::E_DEFERRAL_OPEN, 422),
            (rdy_engine::E_PROPOSAL_NOT_READY, 422),
            (rdy_engine::E_VIEW_CLOSED, 422),
        ] {
            assert_eq!(status_of(code), status, "{code}");
        }
    }
}
