//! The `rdy_inbox` dispatcher MCP and the daemon share, the ack write and the inbox paragraph of
//! `rdy_schema` (plan M2e2, spec §10.2; D-TPZ0W, D-Z0EMP).
use super::read::{DEFAULT_MAX_TOKENS, bad_cursor, cursor_text, parse_cursor};
use super::sub::subscribe;
use crate::engine::write_typed;
use crate::{Engine, Rdy, RdyError, Session};
use rdy_store::inbox::InboxCursor;
use serde::Deserialize;
use serde_json::{Value, json};

/// `rdy_schema`'s inbox paragraph.
pub const INBOX_DOC: &str = "rdy_inbox {since?, ack?, max_tokens?} reads this session's inbox from its cursor: at most 200 entries {event, ids, line}, then the new cursor. ack: true moves the cursor past them. Entries: landings in your areas (rdy_session areas, else the areas of the blocks you change), your views rebased with conflicts or closed, alternative sets your views belong to, other sessions' intents on your blocks or areas, review requests you may claim (reviewer role), dismissals of your tensions, a new proposal version or an advised verdict that waits for the person's answer (pending_added: read rdy_pending), tensions opened on, re-keyed on or gone from your in-flight changes, with their cause, and suspects new on or gone from your blocks. more: true means call again. A new session starts at the head. Notifications on rdy://inbox are best effort; the inbox is the reliable path.";

/// `rdy_inbox`'s input.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxRequest {
    /// The cursor to read from (`{segment}.{offset}.{skip}`); the persisted cursor when absent.
    pub since: Option<String>,
    /// Moves the persisted cursor past the page.
    #[serde(default)]
    pub ack: bool,
    /// The page's token budget; `DEFAULT_MAX_TOKENS` when absent.
    pub max_tokens: Option<usize>,
}

impl Rdy {
    /// Moves the cursor of the session (`actor`, `run`) to `next`, if `next` is past it. Re-reads
    /// the ref, so a racing ack of an older cursor never moves it back. True when it wrote; a
    /// session with no subscription has nothing to move.
    pub fn ack_inbox(
        &mut self,
        actor: &str,
        run: Option<&str>,
        next: InboxCursor,
    ) -> Result<bool, RdyError> {
        let persisted = self.read().reader().inbox(actor, run)?;
        let Some(mut sub) = persisted else {
            return Ok(false);
        };
        if next <= sub.cursor {
            return Ok(false);
        }
        sub.cursor = next;
        self.put_subscription(&sub)?;
        Ok(true)
    }
}

/// Reads one page of session `s`'s inbox: `{entries, cursor, more}`, plus `acked: true` when
/// `ack` moved the cursor past the page. A session with no subscription is subscribed at the
/// head first, and the reply is `{entries: [], cursor, more: false, started: true}`.
pub fn run_inbox(engine: &dyn Engine, s: &Session, req: InboxRequest) -> Result<Value, RdyError> {
    let since = match req.since.as_deref() {
        Some(t) => Some(parse_cursor(t).ok_or_else(|| bad_cursor(t))?),
        None => None,
    };
    let st = engine.read();
    let Some(sub) = st.reader().inbox(&s.actor, s.run.as_deref())? else {
        let sub = subscribe(engine, s, None)?;
        return Ok(json!({
            "entries": [],
            "cursor": cursor_text(&sub.cursor),
            "more": false,
            "started": true,
        }));
    };
    let page = st.inbox_page(&sub, since, req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS))?;
    let mut v = json!({
        "entries": page.entries,
        "cursor": page.cursor,
        "more": page.more,
    });
    if req.ack {
        let (actor, run, next) = (s.actor.clone(), s.run.clone(), page.next);
        write_typed(engine, move |r| r.ack_inbox(&actor, run.as_deref(), next))?;
        v["acked"] = json!(true);
    }
    Ok(v)
}
