//! Inbox subscriptions (plan M2e2): the ref `inbox/<actor>/<run>` holds one session's cursor,
//! declared areas and role. The tension and suspect moves it reads are logged in their own
//! batches (founder decision C).
use crate::error::StoreError;
use crate::segment::Location;
use crate::store::encode;
use crate::{Store, StoreReader};
use serde::{Deserialize, Serialize};

/// The ref prefix of inbox subscriptions: `inbox/<actor>/<run>` holds one session's `InboxSub`.
pub const INBOX_REF: &str = "inbox/";

/// The roles an `InboxSub` may hold.
const ROLES: [&str; 3] = ["reader", "writer", "reviewer"];

/// Where a session's inbox stands in the log: the batch end `at`, and how many entries of the
/// batch that ends there the session has already read (`skip`). Ordered by `at`, then `skip`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InboxCursor {
    pub at: Location,
    pub skip: u32,
}

/// One session's subscription: its cursor, declared areas and role (spec §10.2). `areas` is
/// sorted and distinct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxSub {
    pub actor: String,
    pub run: Option<String>,
    pub cursor: InboxCursor,
    pub areas: Vec<String>,
    pub role: String,
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
    } else if !sorted_distinct(&s.areas) {
        "its lists are not sorted and distinct".to_string()
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
    /// Each write appends the value (cursor, areas and role) to the append-only log, as any ref
    /// write does. The inbox reads tension and suspect moves from their own batches (founder
    /// decision C, 2026-10-12), so an ack writes only the cursor. An ack that changes nothing
    /// writes nothing (ledger O5).
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
