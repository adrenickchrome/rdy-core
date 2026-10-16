//! F2 (D-DKJS5, Task 17): every approval path waits until each exception of the version has a
//! confirm answer. A test that lands a proposed decision proposes it with a summary (a version is
//! approved through a card, and readiness needs one: D-4PG9Y, D-3JZP4), then the person confirms
//! each exception through `Rdy::answer`, the real path.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::{ProposalInfo, Rdy, Session};

/// The one summary a test gives a version that lands a proposed decision.
pub const TEST_SUMMARY: &str = "test summary";

/// Confirms each exception that stands beside version `p` in the queue of the human session
/// `s`, as `s` answering for itself (`Rdy::answer`, option `confirm`, no relay). The version
/// must be ready, or its exceptions are no items.
pub fn confirm_exceptions(r: &mut Rdy, s: &Session, p: &ProposalInfo) {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending");
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        r.answer(
            s,
            &s.actor,
            AnswerRequest {
                r#ref: item.clone(),
                option: "confirm".into(),
                card_hash: None,
                note: None,
                relay: RelayInput::default(),
                brief: None,
            },
        )
        .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}
