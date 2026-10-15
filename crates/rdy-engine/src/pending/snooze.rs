//! Snoozes (F4): "Later" hides an item until the chat session changes.
//!
//! A snooze is an `ANSWER` of act `later`. The ref `pending/<person>/<item ref>` names the
//! person's latest one. The item ref carries the item's version, so a snooze ends in two ways:
//! the chat session changes (`session_ref` differs), or the item gets a new version (its ref
//! differs). The CLI has no Later, so a CLI snooze cannot exist and a query with no
//! `session_ref` hides nothing.
use super::item::Item;
use super::queue::PendingQuery;
use crate::ReadState;
use rdy_model::AnswerAct;

/// `items` without the ones `q.person` snoozed in the session `q.session_ref`, and how many that
/// hid. An item is hidden when `pending/<person>/<item ref>` names a `later` answer whose
/// `relay_evidence.session_ref` equals `q.session_ref`. A snooze that cannot be read hides
/// nothing: the person sees the card again.
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
            a.act == AnswerAct::Later
                && a.item == item
                && a.relay_evidence.is_some_and(|e| e.session_ref == session)
        }
        _ => false,
    }
}
