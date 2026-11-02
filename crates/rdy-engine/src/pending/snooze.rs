//! Snoozes (F4): "Later" hides an item until the chat session changes.
//!
//! A snooze is an `ANSWER` of act `later`. The ref `pending/<person>/<item ref>` names the
//! person's latest one. The item ref carries the item's version, so a snooze ends in two ways:
//! the chat session changes (`session_ref` differs), or the item gets a new version (its ref
//! differs). The CLI has no Later, so a CLI snooze cannot exist and a query with no
//! `session_ref` hides nothing.
//!
//! A relayed snooze names its chat session in `relay_evidence.session_ref`. An elicited snooze
//! (D-WPE4R, O9) has no relay: the run of the session that elicited it, `person.run`, names its
//! chat session.
use super::item::Item;
use super::queue::PendingQuery;
use crate::ReadState;
use rdy_model::{AnswerAct, EvidenceKind};

/// `items` without the ones `q.person` snoozed in the session `q.session_ref`, and how many that
/// hid. An item is hidden when `pending/<person>/<item ref>` names a `later` answer whose chat
/// session (its `relay_evidence.session_ref`, or for a local elicited snooze its `person.run`)
/// equals `q.session_ref`. A snooze that cannot be read hides nothing: the person sees the card
/// again.
pub(crate) fn hide_snoozed(
    st: &ReadState,
    q: &PendingQuery,
    items: Vec<Item>,
) -> (Vec<Item>, usize) {
    let Some(session) = q.session_ref.as_deref() else {
        return (items, 0);
    };
    let before = items.len();
    let shown: Vec<Item> = items
        .into_iter()
        .filter(|i| !snoozed_in(st, &q.person, session, &i.r#ref))
        .collect();
    let hidden = before - shown.len();
    (shown, hidden)
}

/// Whether `person` snoozed `item` in chat session `session`.
fn snoozed_in(st: &ReadState, person: &str, session: &str, item: &str) -> bool {
    match st.reader().snooze_of(person, item) {
        Ok(Some((_, a))) => {
            let in_session = match &a.relay_evidence {
                Some(e) => e.session_ref == session,
                None => {
                    a.evidence_kind == Some(EvidenceKind::Elicited)
                        && a.person.run.as_deref() == Some(session)
                }
            };
            a.act == AnswerAct::Later && a.item == item && in_session
        }
        _ => false,
    }
}
