//! Inbox subscriptions (plan M2e2): the ref `inbox/<actor>/<run>` holds one session's cursor,
//! declared areas, role and the tensions and suspects it has seen. Nothing per event is stored.
use crate::error::StoreError;
use crate::segment::Location;
use crate::store::encode;
use crate::{Store, StoreReader};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};

/// The ref prefix of inbox subscriptions: `inbox/<actor>/<run>` holds one session's `InboxSub`.
pub const INBOX_REF: &str = "inbox/";

/// The most ids each `seen_*` set of an `InboxSub` holds.
pub const MAX_SEEN: usize = 1000;

/// The roles an `InboxSub` may hold.
const ROLES: [&str; 3] = ["reader", "writer", "reviewer"];

/// Where a session's inbox stands in the log: the batch end `at`, and how many entries of the
/// batch that ends there the session has already read (`skip`). Ordered by `at`, then `skip`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InboxCursor {
    pub at: Location,
    pub skip: u32,
}

/// One session's subscription: its cursor, declared areas and role, and the ids of the open
/// tensions on its in-flight changes and of the suspects on its blocks at its last ack.
/// `areas`, `seen_tensions` and `seen_suspects` are sorted and distinct; each `seen_*` holds at
/// most `MAX_SEEN` ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxSub {
    pub actor: String,
    pub run: Option<String>,
    pub cursor: InboxCursor,
    pub areas: Vec<String>,
    pub role: String,
    pub seen_tensions: Vec<Hash>,
    pub seen_suspects: Vec<Hash>,
}

/// The ref of the session (`actor`, `run`): `inbox/<actor>/<run>`, with an empty last segment for
/// no run. A run never holds `/`, so the name splits at its last `/`.
pub fn inbox_ref(actor: &str, run: Option<&str>) -> String {
    format!("{INBOX_REF}{actor}/{}", run.unwrap_or(""))
}

fn sorted_distinct<T: Ord>(v: &[T]) -> bool {
    v.windows(2).all(|w| w[0] < w[1])
}

/// The rule `s`, stored under the ref `name`, breaks, if any: the one rule list `put_inbox` and
/// fsck apply. The actor is not empty, and a run, when present, is not empty and holds no `/`
/// (as `Session::agent` makes them), so one ref name names one session only.
fn invalid(name: &str, s: &InboxSub) -> Option<String> {
    let what = if inbox_ref(&s.actor, s.run.as_deref()) != name {
        "its value names another session".to_string()
    } else if !ROLES.contains(&s.role.as_str()) {
        format!("role {:?} is not reader, writer or reviewer", s.role)
    } else if !(sorted_distinct(&s.areas)
        && sorted_distinct(&s.seen_tensions)
        && sorted_distinct(&s.seen_suspects))
    {
        "its lists are not sorted and distinct".to_string()
    } else if s.seen_tensions.len() > MAX_SEEN || s.seen_suspects.len() > MAX_SEEN {
        format!("more than {MAX_SEEN} seen entries")
    } else if s.actor.is_empty() {
        "its actor is empty".to_string()
    } else if s
        .run
        .as_deref()
        .is_some_and(|r| r.is_empty() || r.contains('/'))
    {
        "its run is empty or holds '/'".to_string()
    } else {
        return None;
    };
    Some(refusal(name, &what))
}

fn refusal(name: &str, what: &str) -> String {
    format!("inbox ref {name:?}: {what}")
}

/// Decodes the value of the ref `name` and applies the rule list.
fn read_sub(name: &str, bytes: &[u8]) -> Result<InboxSub, StoreError> {
    let s: InboxSub = postcard::from_bytes(bytes)
        .map_err(|_| StoreError::Decode(refusal(name, "its value does not decode")))?;
    match invalid(name, &s) {
        Some(m) => Err(StoreError::Decode(m)),
        None => Ok(s),
    }
}

impl Store {
    /// Writes the session's subscription: the ref `inbox/<actor>/<run>`, in one batch. Refuses a
    /// value that breaks the rule list with a `StoreError::Refused`; nothing is written then.
    ///
    /// Each write appends the whole value, seen sets included, to the append-only log. Old values
    /// stay in the log, as old block versions do. That is not history (D-TPZ0W, ruling task-1
    /// §1): no read surface, journal or replay reads a past ack. Reclaiming log space is later
    /// store work. An ack that changes nothing writes nothing (ledger O5).
    pub fn put_inbox(&mut self, sub: &InboxSub) -> Result<(), StoreError> {
        let name = inbox_ref(&sub.actor, sub.run.as_deref());
        if let Some(m) = invalid(&name, sub) {
            return Err(StoreError::Refused(m));
        }
        let items = self.ref_items_pub(vec![(name, encode(sub))]);
        self.write(items)?;
        Ok(())
    }
}

impl StoreReader {
    /// The subscription of the session (`actor`, `run`), if it has one.
    pub fn inbox(&self, actor: &str, run: Option<&str>) -> Result<Option<InboxSub>, StoreError> {
        let name = inbox_ref(actor, run);
        self.get_ref(&name)?
            .map(|bytes| read_sub(&name, &bytes))
            .transpose()
    }

    /// Every subscription, in ref-name order.
    pub fn inboxes(&self) -> Result<Vec<InboxSub>, StoreError> {
        self.refs_with_prefix(INBOX_REF)?
            .into_iter()
            .map(|(name, bytes)| read_sub(&name, &bytes))
            .collect()
    }

    /// Every `inbox/` ref decodes to a subscription that keeps the rule list. Returns how many
    /// refs there are.
    pub(crate) fn check_inboxes(&self) -> Result<usize, StoreError> {
        Ok(self.inboxes()?.len())
    }
}
